//! Headless layout checker (tests only).
//!
//! No window and no screenshots: [`run_frame`] runs one full console frame
//! (`GuiApp::draw_frame`, the same code the window runs) through
//! `egui::Context::run` with a `RawInput` of a given window size, then reads
//! the painted `FullOutput.shapes` and reports
//!
//! * [`Kind::ClipOverflow`]: a text (or shape) sticks out of its clip rect by
//!   more than [`CLIP_TOLERANCE`] px, i.e. it is cut off. Vertical scroll
//!   areas are exempt on the vertical axis (a half-visible row is how they
//!   work), but not horizontally;
//! * [`Kind::Overlap`]: two texts overlap by more than [`OVERLAP_TOLERANCE`] px
//!   on both axes;
//! * [`Kind::OffScreen`]: any painted content beyond the window.
//!
//! Everything is measured in egui points. `size` of [`run_frame`] is the
//! screen rect egui lays the console out in (the window's inner size in
//! points), `pixels_per_point` is the interface scale (`[gui] ui_scale` on a
//! 1x display), which scales the fonts' pixel snapping.
//!
//! What it cannot see: text that is *not* clipped but spills over a
//! neighbouring plate (it only sees text against text), shapes on different
//! egui layers (tooltips and dialogs are not open in the scenarios), and
//! anything the app draws with a custom clip rect equal to no scroll area.
//! Nothing here touches the audio device, the network or the user's config.

use eframe::egui::{
    self, epaint::ClippedShape, pos2, vec2, Context, Pos2, RawInput, Rect, Shape, Vec2,
};

use crate::config::Config;
use crate::gui::app::{ServerStatus, SoundEntry};
use crate::gui::groups::{Library, SceneItem};
use crate::gui::hotkeys_ui::test_support::harness;
use crate::gui::GuiApp;

/// How far content may leave its clip rect (or the window) before it counts.
pub const CLIP_TOLERANCE: f32 = 1.0;
/// Texts must overlap by more than this on both axes to count.
pub const OVERLAP_TOLERANCE: f32 = 2.0;
/// Frames run before the one that is analysed (windows and sizing passes).
const SETTLE_FRAMES: usize = 3;

/// What is wrong with an element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Content sticks out of its clip rect: it is cut off.
    ClipOverflow,
    /// Two texts are painted over each other.
    Overlap,
    /// Content lies (partly) outside the window.
    OffScreen,
    /// egui's own debug warning about two widgets sharing an id (painted as a
    /// red text in debug builds). Not geometry, but it shows up exactly when a
    /// layout draws the same thing twice or collapses.
    IdClash,
    /// A control that must be usable without scrolling has no fully visible
    /// occurrence: it is cut off, scrolled out of view or not painted at all.
    Hidden,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::ClipOverflow => "cut off by clip",
            Kind::Overlap => "text overlap",
            Kind::OffScreen => "off screen",
            Kind::IdClash => "widget id clash",
            Kind::Hidden => "required control not visible",
        }
    }
}

/// One problem found in a frame.
#[derive(Debug, Clone)]
pub struct Finding {
    pub kind: Kind,
    /// The offending element: its text, or the shape kind.
    pub what: String,
    /// Painted bounds of the element.
    pub rect: Rect,
    /// Clip rect it was painted with.
    pub clip: Rect,
    /// The second text, for [`Kind::Overlap`].
    pub other: Option<(String, Rect)>,
}

/// A [`Finding`] with the frame it came from; `Display` is the line the tests
/// print.
#[derive(Debug, Clone)]
pub struct Violation {
    pub finding: Finding,
    pub window: Vec2,
    pub pixels_per_point: f32,
}

fn fmt_rect(r: Rect) -> String {
    if !r.is_finite() {
        return "n/a".to_string();
    }
    format!(
        "[{:.1},{:.1} .. {:.1},{:.1}] {:.1}x{:.1}",
        r.min.x,
        r.min.y,
        r.max.x,
        r.max.y,
        r.width(),
        r.height()
    )
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let x = &self.finding;
        write!(
            f,
            "{}: {:?} rect={} clip={}",
            x.kind.label(),
            x.what,
            fmt_rect(x.rect),
            fmt_rect(x.clip)
        )?;
        if let Some((text, rect)) = &x.other {
            write!(f, " overlaps {:?} rect={}", text, fmt_rect(*rect))?;
        }
        write!(
            f,
            " | window {:.0}x{:.0} @{:.1}",
            self.window.x, self.window.y, self.pixels_per_point
        )
    }
}

/// Result of one analysed frame.
#[derive(Debug, Clone)]
pub struct FrameReport {
    pub pixels_per_point: f32,
    pub violations: Vec<Violation>,
    /// Painted text shapes and all leaf shapes, for sanity checks.
    pub text_count: usize,
    pub shape_count: usize,
    pub elapsed: std::time::Duration,
}

// ---------------------------------------------------------------------------
// Analysis of painted shapes (pure: no egui context needed except the shapes)
// ---------------------------------------------------------------------------

/// A painted leaf: a text or any other shape, with the clip it was painted in.
struct Item {
    text: Option<String>,
    kind: &'static str,
    bounds: Rect,
    clip: Rect,
}

fn shape_kind(shape: &Shape) -> &'static str {
    match shape {
        Shape::Noop => "noop",
        Shape::Vec(_) => "group",
        Shape::Circle(_) => "circle",
        Shape::Ellipse(_) => "ellipse",
        Shape::LineSegment { .. } => "line",
        Shape::Path(_) => "path",
        Shape::Rect(_) => "rect",
        Shape::Text(_) => "text",
        Shape::Mesh(_) => "mesh",
        Shape::QuadraticBezier(_) => "bezier",
        Shape::CubicBezier(_) => "bezier",
        Shape::Callback(_) => "callback",
    }
}

fn collect(shape: &Shape, clip: Rect, out: &mut Vec<Item>) {
    match shape {
        Shape::Noop | Shape::Callback(_) => {}
        Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, clip, out)),
        Shape::Text(text) => {
            let content = text.galley.text().to_string();
            if content.trim().is_empty() {
                return;
            }
            let ink = text.visual_bounding_rect();
            let bounds = if ink.is_positive() && ink.is_finite() {
                ink
            } else {
                text.galley.rect.translate(text.pos.to_vec2())
            };
            out.push(Item {
                text: Some(content),
                kind: "text",
                bounds,
                clip,
            });
        }
        other => {
            let bounds = other.visual_bounding_rect();
            if bounds.is_finite() && bounds.is_positive() {
                out.push(Item {
                    text: None,
                    kind: shape_kind(other),
                    bounds,
                    clip,
                });
            }
        }
    }
}

/// `true` when `clip` is the content clip of a vertical scroll area.
///
/// A scroll area clips its content vertically to its viewport grown by
/// `margin` (egui's `clip_rect_margin`) and leaves the horizontal clip of the
/// parent. `regions` are the rects of widgets and uis of the frame (the
/// viewport is always one of them: the area's own ui); a clip counts when its
/// y range is exactly some rect's y range plus the margin on both sides and
/// its x range covers that rect. With `margin == 0` the clip must equal the
/// rect's y range (explicit regions in tests).
fn is_scroll_clip(clip: Rect, regions: &[Rect], margin: f32) -> bool {
    const EPS: f32 = 0.6;
    regions.iter().any(|r| {
        (clip.min.y - (r.min.y - margin)).abs() <= EPS
            && (clip.max.y - (r.max.y + margin)).abs() <= EPS
            && clip.min.x <= r.min.x + 1.0
            && clip.max.x >= r.max.x - 1.0
    })
}

fn shorten(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    if flat.chars().count() > 48 {
        let head: String = flat.chars().take(47).collect();
        format!("{head}…")
    } else {
        flat
    }
}

/// How far `inner` leaves `outer`, per side, in points (0 when inside).
fn overshoot(inner: Rect, outer: Rect) -> Vec2 {
    let x = (outer.min.x - inner.min.x)
        .max(inner.max.x - outer.max.x)
        .max(0.0);
    let y = (outer.min.y - inner.min.y)
        .max(inner.max.y - outer.max.y)
        .max(0.0);
    vec2(x, y)
}

/// Check painted shapes. `screen` is the window rect; `scroll_regions` are
/// widget / ui rects among which the viewports of the scroll areas are found
/// (see [`is_scroll_clip`]; their content may leave the clip and the window
/// vertically, because it scrolls). Pure function of its input, so it is
/// tested on synthetic frames. Returns the findings, the number of texts and
/// the number of painted leaves.
///
/// `required` are labels (case-insensitive) of controls that must be usable
/// without scrolling: each needs at least one occurrence that lies entirely
/// inside the window and its clip. Scrolling does not excuse them (they are
/// exempt from the scroll rule of the other checks and get [`Kind::Hidden`]
/// instead).
pub fn analyze(
    shapes: &[ClippedShape],
    screen: Rect,
    scroll_regions: &[Rect],
    scroll_margin: f32,
    required: &[&str],
) -> (Vec<Finding>, usize, usize) {
    let mut items = Vec::new();
    for clipped in shapes {
        collect(&clipped.shape, clipped.clip_rect, &mut items);
    }
    let mut findings = Vec::new();

    // egui paints its id-clash warnings as ordinary texts: report them as what
    // they are and keep them out of the geometry checks.
    items.retain(|item| match item.text.as_deref() {
        Some(t) if t.starts_with('\u{1F525}') => {
            findings.push(Finding {
                kind: Kind::IdClash,
                what: shorten(t),
                rect: item.bounds,
                clip: item.clip,
                other: None,
            });
            false
        }
        _ => true,
    });
    let text_count = items.iter().filter(|i| i.text.is_some()).count();

    // Plain shapes (plates, strokes, meter segments) are aggregated: one
    // finding per kind of problem and clip rect, with the count and the union
    // of the offenders, so a cut-off row of 40 meter segments is one line.
    let mut shape_off: Option<(usize, Rect, Rect)> = None;
    let mut shape_cut: Vec<(usize, Rect, Rect)> = Vec::new();

    let is_required = |item: &Item| {
        item.text
            .as_deref()
            .is_some_and(|t| required.iter().any(|r| r.eq_ignore_ascii_case(t.trim())))
    };

    for item in &items {
        // Required controls are judged by the `Hidden` rule below only.
        if is_required(item) {
            continue;
        }
        let scroll = is_scroll_clip(item.clip, scroll_regions, scroll_margin);
        let is_text = item.text.is_some();

        // Off screen: any content beyond the window. Plain shapes get a
        // little more room: strokes are painted half a line outside.
        let screen_tol = if is_text { CLIP_TOLERANCE } else { 2.0 };
        let out = overshoot(item.bounds, screen);
        // Scrolled-away content is below the window by design.
        let out_y = if scroll { 0.0 } else { out.y };
        if out.x > screen_tol || out_y > screen_tol {
            if is_text {
                findings.push(Finding {
                    kind: Kind::OffScreen,
                    what: shorten(item.text.as_deref().unwrap_or("")),
                    rect: item.bounds,
                    clip: item.clip,
                    other: None,
                });
            } else {
                let e = shape_off.get_or_insert((0, item.bounds, item.clip));
                e.0 += 1;
                e.1 = e.1.union(item.bounds);
            }
            continue;
        }

        // Cut off: texts, and filled rects (plates, buttons); thin strokes,
        // paths and meshes legitimately run to their clip.
        if !is_text && item.kind != "rect" {
            continue;
        }
        let over = overshoot(item.bounds, item.clip);
        let bad_x = over.x > CLIP_TOLERANCE;
        let bad_y = over.y > CLIP_TOLERANCE && !scroll;
        if bad_x || bad_y {
            if is_text {
                findings.push(Finding {
                    kind: Kind::ClipOverflow,
                    what: shorten(item.text.as_deref().unwrap_or("")),
                    rect: item.bounds,
                    clip: item.clip,
                    other: None,
                });
            } else if let Some(e) = shape_cut.iter_mut().find(|e| e.2 == item.clip) {
                e.0 += 1;
                e.1 = e.1.union(item.bounds);
            } else {
                shape_cut.push((1, item.bounds, item.clip));
            }
        }
    }
    if let Some((count, rect, clip)) = shape_off {
        findings.push(Finding {
            kind: Kind::OffScreen,
            what: format!("{count} shape(s) without text"),
            rect,
            clip,
            other: None,
        });
    }
    for (count, rect, clip) in shape_cut {
        findings.push(Finding {
            kind: Kind::ClipOverflow,
            what: format!("{count} plate(s) without text"),
            rect,
            clip,
            other: None,
        });
    }

    for name in required {
        let fully_visible = |item: &Item| {
            let tol = CLIP_TOLERANCE;
            overshoot(item.bounds, screen).max_elem() <= tol
                && overshoot(item.bounds, item.clip).max_elem() <= tol
        };
        let mut best: Option<&Item> = None;
        let mut ok = false;
        for item in items.iter().filter(|i| {
            i.text
                .as_deref()
                .is_some_and(|t| name.eq_ignore_ascii_case(t.trim()))
        }) {
            ok |= fully_visible(item);
            best.get_or_insert(item);
        }
        if ok {
            continue;
        }
        findings.push(match best {
            Some(item) => Finding {
                kind: Kind::Hidden,
                what: format!("{name} (cut off or scrolled out of view)"),
                rect: item.bounds,
                clip: item.clip,
                other: None,
            },
            None => Finding {
                kind: Kind::Hidden,
                what: format!("{name} (not painted at all)"),
                rect: Rect::NOTHING,
                clip: screen,
                other: None,
            },
        });
    }

    // Text over text. Only what is actually visible counts: a text cut away
    // completely by its clip is a ClipOverflow, not an overlap.
    let texts: Vec<(&Item, Rect)> = items
        .iter()
        .filter(|i| i.text.is_some())
        .map(|i| (i, i.bounds.intersect(i.clip)))
        .filter(|(_, visible)| visible.is_positive())
        .collect();
    for (i, (a, a_vis)) in texts.iter().enumerate() {
        for (b, b_vis) in texts.iter().skip(i + 1) {
            let both = a_vis.intersect(*b_vis);
            if both.width() <= OVERLAP_TOLERANCE || both.height() <= OVERLAP_TOLERANCE {
                continue;
            }
            // The same text painted twice in the same place (a shadow, a
            // redraw) is not two elements.
            let same_text = a.text == b.text;
            let same_place = (a.bounds.min - b.bounds.min).abs().max_elem() <= 1.5
                && (a.bounds.max - b.bounds.max).abs().max_elem() <= 1.5;
            if same_text && same_place {
                continue;
            }
            findings.push(Finding {
                kind: Kind::Overlap,
                what: shorten(a.text.as_deref().unwrap_or("")),
                rect: a.bounds,
                clip: a.clip,
                other: Some((shorten(b.text.as_deref().unwrap_or("")), b.bounds)),
            });
        }
    }

    (findings, text_count, items.len())
}

// ---------------------------------------------------------------------------
// Running frames
// ---------------------------------------------------------------------------

fn raw_input(size: Vec2) -> RawInput {
    let screen = Rect::from_min_size(Pos2::ZERO, size);
    let mut info = egui::ViewportInfo {
        native_pixels_per_point: Some(1.0),
        ..Default::default()
    };
    info.inner_rect = Some(screen);
    let mut input = RawInput {
        screen_rect: Some(screen),
        ..Default::default()
    };
    input.viewports.insert(egui::ViewportId::ROOT, info);
    input
}

/// Run `frames` frames of `run` on `ctx` (a window of `size` points) and
/// analyse the last one.
pub fn analyze_frames(
    ctx: &Context,
    size: Vec2,
    frames: usize,
    required: &[&str],
    mut run: impl FnMut(&Context),
) -> FrameReport {
    let started = std::time::Instant::now();
    let mut output = None;
    let mut regions: Vec<Rect> = Vec::new();
    for _ in 0..frames.max(1) {
        regions.clear();
        output = Some(ctx.run(raw_input(size), |ctx| {
            run(ctx);
            // Every ui registers a rect; a scroll area's own ui is the one its
            // content clip is derived from.
            ctx.viewport(|vp| {
                for (_, widgets) in vp.this_pass.widgets.layers() {
                    regions.extend(widgets.iter().map(|w| w.rect));
                }
            });
        }));
    }
    let margin = ctx.style().visuals.clip_rect_margin;
    let output = output.expect("at least one frame");
    let screen = Rect::from_min_size(Pos2::ZERO, size);
    let (findings, text_count, shape_count) =
        analyze(&output.shapes, screen, &regions, margin, required);
    let pixels_per_point = ctx.pixels_per_point();
    FrameReport {
        pixels_per_point,
        violations: findings
            .into_iter()
            .map(|finding| Violation {
                finding,
                window: size,
                pixels_per_point,
            })
            .collect(),
        text_count,
        shape_count,
        elapsed: started.elapsed(),
    }
}

/// What the console shows: library, scenes and server state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryKind {
    Empty,
    /// Three sounds with long names in different groups.
    LongNames,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scenario {
    pub library: LibraryKind,
    pub scenes: usize,
    pub server_running: bool,
}

impl Scenario {
    pub const DEFAULT: Scenario = Scenario {
        library: LibraryKind::Empty,
        scenes: 0,
        server_running: false,
    };

    /// Controls the operator must reach without scrolling in this state, at
    /// every supported window size.
    pub fn required_controls(&self) -> Vec<&'static str> {
        let mut required = vec!["STOP ALL", "MUTE", "ADD SOUND"];
        if self.server_running {
            required.extend(["STOP SERVER", "COPY LINK"]);
        } else {
            required.push("START SERVER");
        }
        required
    }

    pub fn name(&self) -> String {
        format!(
            "{} sounds, {} scenes, server {}",
            match self.library {
                LibraryKind::Empty => "no",
                LibraryKind::LongNames => "3 long",
            },
            self.scenes,
            if self.server_running {
                "running"
            } else {
                "stopped"
            }
        )
    }
}

const LONG_NAMES: [(&str, &str, &str); 3] = [
    (
        "/fake/storm.ogg",
        "Thunderstorm over the Misty Mountains at midnight (long loop)",
        "Weather and the elements",
    ),
    (
        "/fake/tavern.ogg",
        "Crowded tavern with a drunken bard singing in the corner",
        "Taverns",
    ),
    (
        "/fake/dragon.wav",
        "Ancient red dragon awakening beneath the Lonely Mountain",
        "Monsters and boss encounters",
    ),
];

const SCENE_NAMES: [&str; 2] = [
    "Ambush on the Old Forest Road",
    "Feast in the Great Hall of the King",
];

/// Fill a fresh headless console for `scenario`. The config is in memory only
/// and is never written: the library is not marked dirty, the update check is
/// off and the first-run wizard is done.
fn build_gui(scenario: Scenario, pixels_per_point: f32) -> (GuiApp, Context) {
    let mut config = Config::default();
    config.gui.onboarding_done = true;
    config.gui.check_updates = false;
    config.gui.global_hotkeys = false;
    config.gui.ui_scale = pixels_per_point;
    let h = harness(config, true);
    let ctx = h.ctx.clone();
    let mut app = h.app;
    crate::gui::install_theme(&ctx);

    let mut lib = Library::new();
    let mut entries = Vec::new();
    if scenario.library == LibraryKind::LongNames {
        for (path, name, group) in LONG_NAMES {
            let group = lib.add_group(group);
            assert!(lib.insert_sound(std::path::Path::new(path), name, 0.8, group));
            let sound = lib.sound(std::path::Path::new(path)).unwrap();
            entries.push(SoundEntry {
                id: crate::audio::engine::SoundId(sound.id),
                name: name.to_string(),
                path: path.into(),
                playing: false,
                volume: 0.8,
                looped: false,
                group: sound.group,
                duration: 125.0,
                sample_rate: 44_100,
            });
        }
    }
    for name in SCENE_NAMES.iter().take(scenario.scenes) {
        let scene = lib.add_scene(name).expect("scene");
        lib.set_scene_color(scene, Some("#a04a4a"));
        for entry in &entries {
            lib.set_item(scene, SceneItem::new(entry.id.0));
        }
    }
    app.library = lib;
    {
        let mut sounds = app.sounds.lock().unwrap();
        let mut order = app.sound_order.lock().unwrap();
        for entry in entries {
            order.push(entry.id);
            sounds.insert(entry.id, entry);
        }
    }
    if scenario.server_running {
        let mut state = app.server_state.lock().unwrap();
        state.status = ServerStatus::Running;
        state.bind_url = Some("http://192.168.100.200:8080".to_string());
        state.local_url = Some("http://127.0.0.1:8080".to_string());
        state.listener_count = 3;
        state.peak_listeners = 5;
    }
    (GuiApp { state: app }, ctx)
}

/// Run the whole console for `scenario` in a window of `size` points at
/// `pixels_per_point` and report every layout violation of the last frame.
pub fn run_frame_with(size: Vec2, pixels_per_point: f32, scenario: Scenario) -> FrameReport {
    let (mut gui, ctx) = build_gui(scenario, pixels_per_point);
    ctx.options_mut(|o| {
        o.zoom_factor = pixels_per_point;
        o.zoom_with_keyboard = false;
    });
    let required = scenario.required_controls();
    let report = analyze_frames(&ctx, size, SETTLE_FRAMES, &required, |ctx| {
        // Never let the debounced save reach the user's config.toml.
        gui.state.library_dirty = false;
        gui.draw_frame(ctx);
        gui.state.library_dirty = false;
    });
    assert!(
        (report.pixels_per_point - pixels_per_point).abs() < 1e-3,
        "the console changed the scale to {}",
        report.pixels_per_point
    );
    report
}

/// [`run_frame_with`] on the default scenario (empty library, server stopped).
pub fn run_frame(size: Vec2, pixels_per_point: f32) -> FrameReport {
    run_frame_with(size, pixels_per_point, Scenario::DEFAULT)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Align2, Color32, FontId};

    const WINDOW: Vec2 = vec2(400.0, 300.0);

    fn check(run: impl FnMut(&Context)) -> Vec<Finding> {
        let ctx = Context::default();
        analyze_frames(&ctx, WINDOW, 2, &[], run).violations_findings()
    }

    impl FrameReport {
        fn violations_findings(&self) -> Vec<Finding> {
            self.violations.iter().map(|v| v.finding.clone()).collect()
        }
    }

    fn paint_text(ui: &egui::Ui, pos: Pos2, text: &str) {
        ui.painter().text(
            pos,
            Align2::LEFT_TOP,
            text,
            FontId::proportional(14.0),
            Color32::WHITE,
        );
    }

    #[test]
    fn a_correct_frame_has_no_violations() {
        let findings = check(|ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                paint_text(ui, pos2(20.0, 20.0), "Library");
                paint_text(ui, pos2(20.0, 60.0), "Server");
                ui.painter().rect_filled(
                    Rect::from_min_size(pos2(20.0, 100.0), vec2(100.0, 20.0)),
                    0.0,
                    Color32::DARK_GRAY,
                );
            });
        });
        assert!(findings.is_empty(), "{findings:#?}");
    }

    #[test]
    fn text_sticking_out_of_its_clip_is_caught() {
        let findings = check(|ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let clip = Rect::from_min_size(pos2(20.0, 20.0), vec2(90.0, 30.0));
                let mut clipped = ui.new_child(egui::UiBuilder::new().max_rect(clip));
                clipped.set_clip_rect(clip);
                paint_text(
                    &clipped,
                    pos2(24.0, 24.0),
                    "A very long label that does not fit",
                );
            });
        });
        assert_eq!(findings.len(), 1, "{findings:#?}");
        assert_eq!(findings[0].kind, Kind::ClipOverflow);
        assert!(findings[0].what.starts_with("A very long"));
    }

    #[test]
    fn content_outside_the_window_is_caught() {
        let findings = check(|ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.set_clip_rect(Rect::EVERYTHING);
                paint_text(ui, pos2(WINDOW.x - 20.0, 20.0), "Mute and more");
            });
        });
        assert!(
            findings.iter().any(|f| f.kind == Kind::OffScreen),
            "{findings:#?}"
        );
    }

    #[test]
    fn overlapping_texts_are_caught_but_touching_ones_are_not() {
        let overlapping = check(|ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                paint_text(ui, pos2(20.0, 20.0), "PROFILE: Default");
                paint_text(ui, pos2(60.0, 22.0), "OFF AIR");
            });
        });
        assert_eq!(overlapping.len(), 1, "{overlapping:#?}");
        assert_eq!(overlapping[0].kind, Kind::Overlap);
        assert_eq!(overlapping[0].other.as_ref().unwrap().0, "OFF AIR");

        let touching = check(|ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                paint_text(ui, pos2(20.0, 20.0), "PROFILE");
                paint_text(ui, pos2(120.0, 20.0), "OFF AIR");
            });
        });
        assert!(touching.is_empty(), "{touching:#?}");
    }

    #[test]
    fn a_text_on_its_own_plate_is_not_an_overlap() {
        let findings = check(|ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let plate = Rect::from_min_size(pos2(20.0, 20.0), vec2(120.0, 28.0));
                ui.painter().rect_filled(plate, 4.0, Color32::DARK_RED);
                paint_text(ui, pos2(28.0, 26.0), "Mute");
            });
        });
        assert!(findings.is_empty(), "{findings:#?}");
    }

    #[test]
    fn rows_cut_by_a_scroll_area_are_fine_but_wide_content_is_not() {
        // A real vertical scroll area whose rows are cut at the bottom edge.
        let fine = check(|ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(100.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for i in 0..40 {
                            ui.label(format!("Row {i}"));
                        }
                    });
            });
        });
        assert!(fine.is_empty(), "{fine:#?}");

        // The same rows, plus one wider than the window (a scroll area never
        // forgives horizontal overflow).
        let wide = check(|ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(100.0)
                    .max_width(120.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add(egui::Label::new("x".repeat(80)).extend());
                        for i in 0..40 {
                            ui.label(format!("Row {i}"));
                        }
                    });
            });
        });
        assert!(
            wide.iter()
                .any(|f| f.kind == Kind::OffScreen && f.what.starts_with("xxx")),
            "{wide:#?}"
        );
    }

    #[test]
    fn explicit_scroll_regions_exempt_vertical_overflow_only() {
        let ctx = Context::default();
        let region = Rect::from_min_size(pos2(20.0, 20.0), vec2(200.0, 40.0));
        let out = ctx.run(raw_input(WINDOW), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(region));
                child.set_clip_rect(region);
                // Half a row below the viewport: normal for a scroll area.
                paint_text(&child, pos2(24.0, 50.0), "Half visible row");
            });
        });
        let screen = Rect::from_min_size(Pos2::ZERO, WINDOW);
        let (without, _, _) = analyze(&out.shapes, screen, &[], 0.0, &[]);
        let (with, _, _) = analyze(&out.shapes, screen, &[region], 0.0, &[]);
        assert_eq!(without.len(), 1, "{without:#?}");
        assert!(with.is_empty(), "{with:#?}");
    }

    #[test]
    fn required_controls_must_be_visible_even_inside_a_scroll_area() {
        let ctx = Context::default();
        let screen = Rect::from_min_size(Pos2::ZERO, WINDOW);
        let region = Rect::from_min_size(pos2(20.0, 20.0), vec2(200.0, 40.0));
        let run = |ctx: &Context, y: f32| {
            ctx.run(raw_input(WINDOW), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(region));
                    child.set_clip_rect(region);
                    paint_text(&child, pos2(24.0, y), "Mute");
                });
            })
        };
        // Fully inside the viewport: fine.
        let out = run(&ctx, 30.0);
        let (ok, _, _) = analyze(&out.shapes, screen, &[region], 0.0, &["MUTE"]);
        assert!(ok.is_empty(), "{ok:#?}");
        // Half scrolled out: a scroll area excuses it for ordinary content, but
        // not for a required control.
        let out = run(&ctx, 52.0);
        let (other, _, _) = analyze(&out.shapes, screen, &[region], 0.0, &[]);
        assert!(other.is_empty(), "{other:#?}");
        let (hidden, _, _) = analyze(&out.shapes, screen, &[region], 0.0, &["Mute"]);
        assert_eq!(hidden.len(), 1, "{hidden:#?}");
        assert_eq!(hidden[0].kind, Kind::Hidden);
        // Not painted at all.
        let (absent, _, _) = analyze(&out.shapes, screen, &[region], 0.0, &["Stop all"]);
        assert_eq!(absent.len(), 1, "{absent:#?}");
        assert!(absent[0].what.contains("not painted"), "{absent:#?}");
    }

    #[test]
    fn violations_print_element_rect_clip_and_window() {
        let findings = check(|ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                paint_text(ui, pos2(20.0, 20.0), "Alpha beta");
                paint_text(ui, pos2(30.0, 22.0), "Gamma delta");
            });
        });
        let v = Violation {
            finding: findings[0].clone(),
            window: WINDOW,
            pixels_per_point: 1.0,
        };
        let line = v.to_string();
        assert!(line.contains("text overlap"), "{line}");
        assert!(line.contains("Alpha beta"), "{line}");
        assert!(line.contains("clip="), "{line}");
        assert!(line.contains("window 400x300 @1.0"), "{line}");
    }

    #[test]
    fn the_console_runs_headless_and_fast() {
        let report = run_frame(vec2(1280.0, 800.0), 1.0);
        // The shell painted real content.
        assert!(report.text_count > 10, "{} texts", report.text_count);
        assert!(report.shape_count > report.text_count);
        assert!(
            report.elapsed.as_secs_f32() < 5.0,
            "three frames took {:?}",
            report.elapsed
        );
    }

    // ---- the matrix --------------------------------------------------------

    const SIZES: [(f32, f32); 5] = [
        (900.0, 600.0),
        (960.0, 620.0),
        (1100.0, 700.0),
        (1280.0, 800.0),
        (1600.0, 900.0),
    ];
    const SCALES: [f32; 2] = [1.0, 1.3];

    fn scenarios() -> Vec<Scenario> {
        vec![
            Scenario::DEFAULT,
            Scenario {
                library: LibraryKind::LongNames,
                ..Scenario::DEFAULT
            },
            Scenario {
                library: LibraryKind::LongNames,
                scenes: 2,
                server_running: false,
            },
            Scenario {
                library: LibraryKind::LongNames,
                scenes: 0,
                server_running: true,
            },
            Scenario {
                library: LibraryKind::Empty,
                scenes: 0,
                server_running: true,
            },
        ]
    }

    /// Every size x scale x state. Prints every violation and fails if any.
    fn matrix() -> Vec<String> {
        let mut lines = Vec::new();
        for scenario in scenarios() {
            for scale in SCALES {
                for (w, h) in SIZES {
                    let report = run_frame_with(vec2(w, h), scale, scenario);
                    for v in &report.violations {
                        lines.push(format!("[{}] {v}", scenario.name()));
                    }
                }
            }
        }
        lines
    }

    #[test]
    #[ignore = "enabled after layout fixes land"]
    fn the_console_has_no_layout_violations_in_any_size_scale_and_state() {
        let lines = matrix();
        for line in &lines {
            println!("{line}");
        }
        assert!(
            lines.is_empty(),
            "{} layout violations (listed above)",
            lines.len()
        );
    }
}

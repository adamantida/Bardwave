//! Top command bar — brand lockup, master volume, live server status.
//!
//! 64px tall, `#0A0A0A`, hairline bottom border, 24px horizontal padding:
//! brand left · master volume centre-left · live signal right.
//!
//! The bar is laid out in two explicit rects: the status cluster is pinned to
//! the right edge and the controls fill what is left of it. How much the
//! controls may spend is decided by the pure [`top_bar_layout`]; the status
//! cluster is never overlapped, whatever the window width.

use crate::gui::app::{ServerStatus, SoundSharedApp};
use crate::gui::profiles_ui::ProfileStyle;
use crate::gui::theme::{micro_label, wordmark, Metrics, Palette};
use crate::gui::widgets::{glyph, level_slider, signal_dot};
use egui::{Align, Layout, Rect, Rounding, Sense, Ui, UiBuilder, Vec2};

// ---- Widths of the bar's items (points) ----
//
// The bar plans its width with the *measured* text widths of the current
// frame ([`Widths::measure`]: the same galleys the widgets paint, at the
// current scale, fonts and style). The constants below are the budget the pure
// [`top_bar_layout`] plans with (tests, no egui context); the test
// `measured_items_fit_their_budgets` keeps them honest.

/// The rose mark of the brand lockup.
const MARK_W: f32 = 18.0;
/// `BARDWAVE` wordmark (18px, strong).
const WORDMARK_W: f32 = 104.0;
/// `BROADCAST CONSOLE` caption under it (9px, tracked), wider than the wordmark.
const CAPTION_W: f32 = 112.0;
/// `MASTER` micro label.
const MASTER_LABEL_W: f32 = 46.0;
/// `100%` readout after the slider.
const MASTER_READOUT_W: f32 = 28.0;
/// `STOP ALL` with its icon: pad + icon + gap + text + pad.
const STOP_ALL_W: f32 = 96.0;
/// `PROFILE` micro label.
const PROFILE_LABEL_W: f32 = 52.0;
/// Horizontal padding of a menu button (`button_padding.x` twice).
const MENU_BTN_PAD: f32 = 2.0 * Metrics::MD;
/// Widest profile name text (18 characters, 11px mono).
const PROFILE_NAME_W: f32 = 128.0;
/// Narrowest profile name text: a couple of characters and the ellipsis.
const PROFILE_NAME_MIN_W: f32 = 36.0;
/// `Help` menu button.
const HELP_W: f32 = 58.0;
/// Status cluster with its second line (`SERVER FAILED — SEE PANEL`).
const STATUS_FULL_W: f32 = 168.0;
/// Status cluster with only the headline (`STARTING`) and the dot.
const STATUS_COMPACT_W: f32 = 84.0;
/// The status dot alone (the last thing the status gives up).
const STATUS_DOT_W: f32 = 7.0;
/// Hairline divider.
const DIVIDER_W: f32 = 1.0;
/// Smallest gap kept between the controls and the status cluster.
const SIDE_GAP: f32 = Metrics::MD;

/// How wide each text-bearing item of the bar is. [`Widths::BUDGET`] for the
/// pure planner, [`Widths::measure`] for a real frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Widths {
    wordmark: f32,
    caption: f32,
    master_label: f32,
    master_readout: f32,
    stop_all: f32,
    profile_label: f32,
    /// The profile name as it would be shown in full (before any cap).
    profile_name: f32,
    help: f32,
    status_full: f32,
    status_compact: f32,
}

impl Widths {
    /// What the layout may count on with egui's bundled fonts.
    pub const BUDGET: Widths = Widths {
        wordmark: WORDMARK_W,
        caption: CAPTION_W,
        master_label: MASTER_LABEL_W,
        master_readout: MASTER_READOUT_W,
        stop_all: STOP_ALL_W,
        profile_label: PROFILE_LABEL_W,
        profile_name: PROFILE_NAME_W,
        help: HELP_W,
        status_full: STATUS_FULL_W,
        status_compact: STATUS_COMPACT_W,
    };

    /// Measure the items of this frame: the galleys the widgets will paint.
    fn measure(ui: &Ui, state: &SoundSharedApp) -> Widths {
        use egui::{TextStyle, TextWrapMode, WidgetText};
        let text = |rich: egui::RichText| -> f32 {
            WidgetText::from(rich)
                .into_galley(
                    ui,
                    Some(TextWrapMode::Extend),
                    f32::INFINITY,
                    TextStyle::Body,
                )
                .size()
                .x
                .ceil()
        };
        let pad = Metrics::MD;
        let status = status_texts(state);
        let headline = text(status_headline_text(&status.headline, status.color));
        let subline = text(status_subline_text(&status.subline));
        let name = text(
            egui::RichText::new(crate::gui::profiles_ui::bar_name(state))
                .font(crate::gui::widgets::glyph_font(11.0)),
        );
        let dot_and_headline = STATUS_DOT_W + Metrics::SM + headline;
        Widths {
            wordmark: text(wordmark("Bardwave")),
            caption: text(micro_label("Broadcast Console").size(9.0)),
            master_label: text(micro_label("Master")),
            master_readout: text(
                egui::RichText::new("100%")
                    .font(egui::FontId::new(11.0, egui::FontFamily::Monospace)),
            ),
            stop_all: (pad
                + STOP_ALL_ICON
                + Metrics::SM
                + text(
                    egui::RichText::new("STOP ALL")
                        .font(crate::gui::theme::mono_font(Metrics::MICRO)),
                )
                + pad)
                .ceil(),
            profile_label: text(micro_label("Profile")),
            profile_name: name,
            help: crate::gui::onboarding::help_button_width(ui),
            status_full: dot_and_headline.max(subline),
            status_compact: dot_and_headline,
        }
    }
}

/// Side of the `STOP ALL` icon.
const STOP_ALL_ICON: f32 = 12.0;

/// Brand lockup variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrandStyle {
    /// Mark, wordmark and the `BROADCAST CONSOLE` caption.
    Full,
    /// Mark and wordmark.
    Compact,
    /// The mark only.
    Mark,
    /// Nothing: the bar has no room for a lockup.
    None,
}

/// A group of controls on the left of the bar, in drawing order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    Brand,
    Master,
    StopAll,
    Rec,
    Profile,
}

/// How the command bar spends its width. Produced by [`top_bar_layout`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TopBarLayout {
    /// Widths the plan was made with.
    pub widths: Widths,
    pub brand: BrandStyle,
    /// Gap on each side of a divider.
    pub gap: f32,
    /// Master volume group (slider and readout).
    pub master: bool,
    /// `MASTER` caption before the slider.
    pub master_label: bool,
    pub master_slider_w: f32,
    /// `STOP ALL` with text (else an icon-only button).
    pub stop_all_label: bool,
    /// Narrow REC button.
    pub rec_compact: bool,
    /// Recordings-folder button after REC.
    pub rec_folder: bool,
    /// The profile control at all.
    pub profile: bool,
    /// `PROFILE` caption before the profile button.
    pub profile_label: bool,
    /// Widest the profile name text may be.
    pub profile_name_w: f32,
    /// Status cluster without its second line.
    pub status_compact: bool,
    /// Status cluster reduced to its dot.
    pub status_dot: bool,
}

impl TopBarLayout {
    fn full(widths: Widths) -> Self {
        Self {
            widths,
            brand: BrandStyle::Full,
            gap: Metrics::XL,
            master: true,
            master_label: true,
            master_slider_w: MASTER_SLIDER_W,
            stop_all_label: true,
            rec_compact: false,
            rec_folder: true,
            profile: true,
            profile_label: true,
            profile_name_w: PROFILE_NAME_W,
            status_compact: false,
            status_dot: false,
        }
    }

    fn brand_w(&self) -> f32 {
        let w = &self.widths;
        match self.brand {
            BrandStyle::Full => MARK_W + Metrics::SM + w.caption.max(w.wordmark),
            BrandStyle::Compact => MARK_W + Metrics::SM + w.wordmark,
            BrandStyle::Mark => MARK_W,
            BrandStyle::None => 0.0,
        }
    }

    fn master_w(&self) -> f32 {
        let label = if self.master_label {
            self.widths.master_label + Metrics::LABEL_GAP
        } else {
            0.0
        };
        label + self.master_slider_w + Metrics::SM + self.widths.master_readout
    }

    fn stop_all_w(&self) -> f32 {
        if self.stop_all_label {
            self.widths.stop_all
        } else {
            Metrics::BTN_H_SM
        }
    }

    fn rec_w(&self) -> f32 {
        let button = if self.rec_compact {
            crate::gui::recorder_ui::REC_BTN_W_COMPACT
        } else {
            crate::gui::recorder_ui::REC_BTN_W
        };
        if self.rec_folder {
            button + Metrics::SM + crate::gui::recorder_ui::REC_FOLDER_W
        } else {
            button
        }
    }

    /// Width of the profile name text: its cap, or less when the name is short.
    pub fn profile_text_w(&self) -> f32 {
        self.profile_name_w.min(self.widths.profile_name)
    }

    fn profile_w(&self) -> f32 {
        let label = if self.profile_label {
            self.widths.profile_label + Metrics::SM
        } else {
            0.0
        };
        label + self.profile_text_w() + MENU_BTN_PAD
    }

    fn status_w(&self) -> f32 {
        if self.status_dot {
            STATUS_DOT_W
        } else if self.status_compact {
            self.widths.status_compact
        } else {
            self.widths.status_full
        }
    }

    /// The groups on the left that are shown, in order, with their widths.
    fn groups(&self) -> Vec<(Group, f32)> {
        let mut out = Vec::with_capacity(5);
        if self.brand != BrandStyle::None {
            out.push((Group::Brand, self.brand_w()));
        }
        if self.master {
            out.push((Group::Master, self.master_w()));
        }
        out.push((Group::StopAll, self.stop_all_w()));
        out.push((Group::Rec, self.rec_w()));
        if self.profile {
            out.push((Group::Profile, self.profile_w()));
        }
        out
    }

    /// Space between the previous group and `group`: a hairline with a gap on
    /// each side, but only a plain gap before the profile (it belongs to REC).
    fn lead(&self, group: Group) -> f32 {
        match group {
            Group::Profile => Metrics::BLOCK_GAP,
            _ => 2.0 * self.gap + DIVIDER_W,
        }
    }

    /// Width of the controls on the left (padding and the groups).
    pub fn left_width(&self) -> f32 {
        let mut width = Metrics::PANEL_PAD;
        for (i, (group, w)) in self.groups().into_iter().enumerate() {
            if i > 0 {
                width += self.lead(group);
            }
            width += w;
        }
        width
    }

    /// Width of the right cluster: help, divider, status and the edge padding.
    pub fn right_width(&self) -> f32 {
        self.widths.help + self.gap + DIVIDER_W + self.gap + self.status_w() + Metrics::PANEL_PAD
    }

    /// Everything the bar needs, including the gap kept between its halves.
    pub fn total_width(&self) -> f32 {
        self.left_width() + SIDE_GAP + self.right_width()
    }
}

/// Where the status cluster and the controls go in a bar of `width` points:
/// `(left rect width, right rect width)`. The two never overlap.
pub fn top_bar_split(layout: &TopBarLayout, width: f32) -> (f32, f32) {
    let right = layout.right_width();
    (width - right - SIDE_GAP, right)
}

/// Give-ups in the order the bar makes them as the window narrows: first the
/// optional text (brand caption, status line) and wide gaps, then the profile
/// name shrinks, the `PROFILE` / `MASTER` captions go, REC gets narrower, the
/// profile name shrinks further, and only then the smaller groups: gaps, master
/// slider, icon-only STOP ALL, the folder button and the wordmark. Past what
/// any supported window needs the bar keeps shedding, secondary things first
/// (the profile, the master volume, the lockup, the status text) and never the
/// two things that matter most when the window is tiny: Help and the status
/// dot on the right, STOP ALL and REC on the left.
const STEPS: &[fn(&mut TopBarLayout)] = &[
    |l| {
        l.brand = BrandStyle::Compact;
        l.status_compact = true;
        l.gap = Metrics::MD;
        l.master_slider_w = MASTER_SLIDER_W_COMPACT;
    },
    |l| l.profile_name_w = 80.0,
    |l| {
        l.profile_label = false;
        l.master_label = false;
    },
    |l| l.rec_compact = true,
    |l| l.profile_name_w = 56.0,
    |l| {
        l.gap = Metrics::SM;
        l.master_slider_w = 80.0;
    },
    |l| l.stop_all_label = false,
    |l| l.rec_folder = false,
    |l| l.master_slider_w = 56.0,
    |l| l.brand = BrandStyle::Mark,
    |l| l.profile_name_w = PROFILE_NAME_MIN_W,
    // Below every window the console supports.
    |l| l.profile = false,
    |l| l.master = false,
    |l| l.brand = BrandStyle::None,
    |l| l.status_dot = true,
];

/// Pick the bar's layout for `width` points from the budget widths: the
/// roomiest form whose items (and the gap before the status) add up to no more
/// than `width`. Pure, so the whole range of window widths is tested without
/// drawing.
pub fn top_bar_layout(width: f32) -> TopBarLayout {
    top_bar_layout_with(Widths::BUDGET, width)
}

/// [`top_bar_layout`] over `widths` (the measured ones in a real frame).
pub fn top_bar_layout_with(widths: Widths, width: f32) -> TopBarLayout {
    let mut layout = TopBarLayout::full(widths);
    for step in STEPS {
        if layout.total_width() <= width {
            break;
        }
        step(&mut layout);
    }
    layout
}

/// Width of the master volume slider in the roomy bar.
const MASTER_SLIDER_W: f32 = 150.0;
/// Width of the master volume slider in the compact bar.
const MASTER_SLIDER_W_COMPACT: f32 = 110.0;

/// Renders the top command bar.
pub fn top_bar(ui: &mut Ui, state: &mut SoundSharedApp) {
    let bar_rect = ui.max_rect();
    ui.painter()
        .rect_filled(bar_rect, Rounding::ZERO, Palette::APP);

    let widths = Widths::measure(ui, state);
    let layout = top_bar_layout_with(widths, bar_rect.width());
    let (left_w, right_w) = top_bar_split(&layout, bar_rect.width());
    let right_rect = Rect::from_min_max(
        egui::pos2(bar_rect.right() - right_w, bar_rect.top()),
        bar_rect.max,
    );
    let left_rect =
        Rect::from_min_size(bar_rect.min, Vec2::new(left_w.max(0.0), bar_rect.height()));
    let gap = layout.gap;

    // Controls: clipped to their rect, and every group is drawn only when the
    // rest of the rect has room for it, so a window narrower than the bar can
    // plan for loses controls instead of painting over the status.
    let mut left = ui.new_child(
        UiBuilder::new()
            .id_salt("top_bar_left")
            .max_rect(left_rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    left.set_clip_rect(left_rect.intersect(ui.clip_rect()));
    // Every gap below is placed by hand.
    left.spacing_mut().item_spacing.x = 0.0;
    left.add_space(Metrics::PANEL_PAD);
    let mut drawn_any = false;
    for (group, group_w) in layout.groups() {
        let lead = if drawn_any { layout.lead(group) } else { 0.0 };
        let room = left_rect.right() - left.min_rect().right();
        if lead + group_w > room + 0.01 {
            break;
        }
        if drawn_any {
            if group == Group::Profile {
                left.add_space(lead);
            } else {
                left.add_space(gap);
                vertical_divider(&mut left);
                left.add_space(gap);
            }
        }
        drawn_any = true;
        match group {
            Group::Brand => brand(&mut left, layout.brand),
            Group::Master => master_volume(&mut left, state, &layout),
            Group::StopAll => quick_clear(&mut left, state, layout.stop_all_label),
            Group::Rec => crate::gui::recorder_ui::top_bar_controls(
                &mut left,
                state,
                layout.rec_compact,
                layout.rec_folder,
            ),
            Group::Profile => crate::gui::profiles_ui::profile_menu(
                &mut left,
                state,
                ProfileStyle {
                    label: layout.profile_label,
                    name_w: layout.profile_text_w(),
                },
            ),
        }
    }
    // The recording readout takes what the fixed controls left over.
    let spare = left_rect.right() - left.min_rect().right();
    crate::gui::recorder_ui::top_bar_readout(&mut left, state, spare);
    #[cfg(test)]
    let left_end = left.min_rect().right();
    #[cfg(test)]
    let left_painted = left.min_rect();

    // Status cluster, pinned to the right edge. It has priority: its rect is
    // exactly what the layout reserved for it and nothing of the controls is
    // ever drawn into it.
    let mut right = ui.new_child(
        UiBuilder::new()
            .id_salt("top_bar_right")
            .max_rect(right_rect)
            .layout(Layout::right_to_left(Align::Center)),
    );
    right.set_clip_rect(right_rect.intersect(ui.clip_rect()));
    right.spacing_mut().item_spacing.x = 0.0;
    right.add_space(Metrics::PANEL_PAD);
    live_status(
        &mut right,
        state,
        layout.status_compact,
        layout.status_dot,
        layout.status_w(),
    );
    right.add_space(gap);
    vertical_divider(&mut right);
    right.add_space(gap);
    // Help sits in a cell as tall as the bar, placed by hand, and centres
    // itself in it: it shares the centre line of the controls on the left
    // whatever the cursor of the right-to-left layout has drifted to.
    let help_w = layout.widths.help;
    let help_cell = Rect::from_min_size(
        egui::pos2(right.min_rect().left() - help_w, right_rect.top()),
        Vec2::new(help_w, right_rect.height()),
    );
    right.allocate_new_ui(
        UiBuilder::new()
            .id_salt("top_bar_help")
            .max_rect(help_cell)
            .layout(Layout::left_to_right(Align::Center)),
        |cell| crate::gui::onboarding::help_menu(cell, state),
    );
    #[cfg(test)]
    {
        let right_start = right.min_rect().left();
        ui.ctx().data_mut(|d| {
            d.insert_temp(egui::Id::new("top_bar_extent"), (left_end, right_start));
            d.insert_temp(
                egui::Id::new("top_bar_rects"),
                [left_rect, right_rect, left_painted, right.min_rect()],
            );
        });
    }

    // Bottom hairline.
    let line = Rect::from_min_size(
        egui::pos2(bar_rect.left(), bar_rect.bottom() - 1.0),
        Vec2::new(bar_rect.width(), 1.0),
    );
    ui.painter()
        .rect_filled(line, Rounding::ZERO, Palette::BORDER);
}

/// Brand lockup: rose signal mark + wordmark + `BROADCAST CONSOLE` subtitle.
fn brand(ui: &mut Ui, style: BrandStyle) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(MARK_W), Sense::hover());
        ui.painter()
            .rect_filled(rect, Rounding::same(2.0), Palette::ROSE_600);
        crate::gui::widgets::paint_icon(
            ui.painter(),
            glyph::WAVE,
            rect.center(),
            13.8,
            Palette::WHITE,
        );
        if style == BrandStyle::Mark {
            return;
        }

        ui.add_space(Metrics::SM);

        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = Metrics::LOCKUP_GAP;
            ui.label(wordmark("Bardwave"));
            if style == BrandStyle::Full {
                ui.label(micro_label("Broadcast Console").size(9.0));
            }
        });
    });
}

/// Master volume: micro label (when there is room), slider, mono readout.
fn master_volume(ui: &mut Ui, state: &mut SoundSharedApp, layout: &TopBarLayout) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        if layout.master_label {
            ui.label(micro_label("Master"));
            ui.add_space(Metrics::LABEL_GAP);
        }

        let mut value = state.get_master_volume();
        let resp = level_slider(ui, &mut value, layout.master_slider_w, "master_top")
            .on_hover_text("Master volume");
        if resp.changed() {
            state.set_master_volume(value);
        }

        ui.add_space(Metrics::SM);
        ui.label(
            egui::RichText::new(format!("{:.0}%", value * 100.0))
                .font(egui::FontId::new(11.0, egui::FontFamily::Monospace))
                .color(Palette::WHITE),
        );
    });
}

/// What is running right now, as far as the "Stop all" button cares.
///
/// Sounds are not the only thing a panic has to silence: the active scene (its
/// delayed starts and emitters), the music layer and the local preview all keep
/// making noise or scheduling it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BoardActivity {
    /// At least one sound row is playing.
    pub sounds: bool,
    /// A scene is active (its emitters and delayed starts are live).
    pub scene: bool,
    /// A playlist plays or is paused.
    pub music: bool,
    /// The headphone preview is running.
    pub preview: bool,
}

impl BoardActivity {
    /// Anything at all that a stop-all would change.
    pub fn any(self) -> bool {
        self.sounds || self.scene || self.music || self.preview
    }
}

/// Visual state of the "Stop all" button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanicState {
    /// Nothing plays: muted, but still readable and still clickable.
    Idle,
    /// Something plays: the button is lit.
    Live,
}

/// Pick the button state from what is running.
pub fn panic_state(activity: BoardActivity) -> PanicState {
    if activity.any() {
        PanicState::Live
    } else {
        PanicState::Idle
    }
}

/// Tooltip of the button for a state.
pub fn panic_tooltip(state: PanicState) -> &'static str {
    match state {
        PanicState::Live => "Stop all sounds immediately (panic)",
        PanicState::Idle => "Stop all sounds immediately (panic) - nothing is playing",
    }
}

/// Colours of the "Stop all" button for a state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanicColors {
    pub fill: egui::Color32,
    pub border: egui::Color32,
    pub text: egui::Color32,
    pub glyph: egui::Color32,
}

/// Colours of the button.
///
/// Live: rose-on-dark like a playing row, white on rose while hovered. Idle:
/// transparent with a hairline border; the text stays zinc-400 so it reads at
/// 4.5:1 on the bar, and the missing fill and rose make it look inactive.
pub fn panic_colors(state: PanicState, hovered: bool) -> PanicColors {
    let (fill, border, text, glyph) = match (state, hovered) {
        (PanicState::Live, false) => (
            Palette::ROSE_950,
            Palette::ROSE_500,
            Palette::ROSE_400,
            Palette::ROSE_400,
        ),
        (PanicState::Live, true) => (
            Palette::ROSE_600,
            Palette::ROSE_500,
            Palette::WHITE,
            Palette::WHITE,
        ),
        (PanicState::Idle, false) => (
            Palette::TRANSPARENT,
            Palette::BORDER,
            Palette::ZINC_400,
            Palette::ZINC_500,
        ),
        (PanicState::Idle, true) => (
            Palette::ROW,
            Palette::BORDER_STRONG,
            Palette::ZINC_300,
            Palette::ZINC_400,
        ),
    };
    PanicColors {
        fill,
        border,
        text,
        glyph,
    }
}

/// Collect what is running from the app (a few flags; the music state is the
/// copy the frame already observed, so nothing is cloned or blocked on).
fn board_activity(state: &SoundSharedApp) -> BoardActivity {
    BoardActivity {
        sounds: state.sounds.lock().unwrap().values().any(|s| s.playing),
        scene: state.director.active_scene().is_some(),
        music: crate::gui::music::is_active(state.music.live()),
        preview: state.lib_ui.preview.is_some(),
    }
}

/// "Stop all": the panic control of the command bar.
///
/// The click always goes out, also when the button looks idle: a panic control
/// that ignores a press because the display thinks nothing plays can never be
/// trusted. The command goes through the Director (see
/// `SoundSharedApp::stop_all_sounds`), so the active scene, emitters, delayed
/// starts, the music layer and the preview stop with the sounds.
fn quick_clear(ui: &mut Ui, state: &mut SoundSharedApp, with_label: bool) {
    let panic = panic_state(board_activity(state));
    let font = crate::gui::theme::mono_font(Metrics::MICRO);
    let label = "STOP ALL";

    let measure = ui
        .painter()
        .layout_no_wrap(label.to_string(), font.clone(), Palette::ZINC_400);
    let icon = 12.0;
    let pad = Metrics::MD;
    let width = if with_label {
        pad + icon + Metrics::SM + measure.size().x + pad
    } else {
        Metrics::BTN_H_SM
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, Metrics::BTN_H_SM), Sense::click());
    let mut resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.clicked() {
        resp.request_focus();
    }
    if resp.fake_primary_click {
        resp.clicked = true;
    }

    let colors = panic_colors(panic, resp.hovered());
    let painter = ui.painter();
    painter.rect(
        rect,
        Rounding::same(Metrics::R_BTN),
        colors.fill,
        egui::Stroke::new(1.0_f32, colors.border),
    );
    let icon_x = if with_label {
        rect.left() + pad + icon / 2.0
    } else {
        rect.center().x
    };
    crate::gui::widgets::paint_icon(
        painter,
        glyph::STOP,
        egui::pos2(icon_x, rect.center().y),
        icon,
        colors.glyph,
    );
    if with_label {
        let galley = painter.layout_no_wrap(label.to_string(), font, colors.text);
        painter.galley(
            egui::pos2(
                rect.left() + pad + icon + Metrics::SM,
                rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            colors.text,
        );
    }

    let resp = resp.on_hover_text(panic_tooltip(panic));
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Stop all"));
    if resp.clicked() {
        state.stop_all_sounds();
    }
}

/// What the status cluster says for the server's current state.
struct StatusTexts {
    color: egui::Color32,
    headline: String,
    subline: String,
    pulse: bool,
}

fn status_texts(state: &SoundSharedApp) -> StatusTexts {
    let server = state.get_server_state();
    let (color, headline, subline, pulse) = match &server.status {
        ServerStatus::Running => (
            Palette::ROSE_500,
            "Live".to_string(),
            format!("Server running · :{}", state.get_server_port()),
            true,
        ),
        ServerStatus::Starting => (
            Palette::AMBER_400,
            "Starting".to_string(),
            "Binding listener".to_string(),
            true,
        ),
        ServerStatus::Error(_) => (
            Palette::ROSE_400,
            "Error".to_string(),
            "Server failed — see panel".to_string(),
            false,
        ),
        ServerStatus::Stopped => (
            Palette::ZINC_500,
            "Off Air".to_string(),
            "Server stopped".to_string(),
            false,
        ),
    };
    StatusTexts {
        color,
        headline,
        subline,
        pulse,
    }
}

/// `LIVE` / `OFF AIR` headline of the status cluster.
fn status_headline_text(headline: &str, color: egui::Color32) -> egui::RichText {
    egui::RichText::new(headline.to_uppercase())
        .font(egui::FontId::new(11.0, egui::FontFamily::Monospace))
        .color(color)
        .extra_letter_spacing(1.0)
        .strong()
}

/// Second line of the status cluster.
fn status_subline_text(subline: &str) -> egui::RichText {
    micro_label(subline).size(9.0).extra_letter_spacing(0.8)
}

/// Right-hand live signal: pulsing dot, `LIVE`/`OFF AIR`, secondary line.
/// Drawn into exactly `width` points, right-aligned; the second line is cut
/// with an ellipsis rather than growing past it. `dot_only` keeps just the dot
/// for a window with no room for words.
fn live_status(ui: &mut Ui, state: &mut SoundSharedApp, compact: bool, dot_only: bool, width: f32) {
    let status = status_texts(state);
    let StatusTexts {
        color,
        headline,
        subline,
        pulse,
    } = status;
    let pulse = pulse && !state.reduced_motion();

    ui.allocate_ui_with_layout(
        Vec2::new(width, ui.available_height()),
        Layout::right_to_left(Align::Center),
        |ui| {
            ui.set_max_width(width);
            if dot_only {
                signal_dot(ui, color, STATUS_DOT_W, pulse);
                return;
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = Metrics::LOCKUP_GAP;
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(status_headline_text(&headline, color));
                    ui.add_space(Metrics::SM);
                    signal_dot(ui, color, STATUS_DOT_W, pulse);
                });
                if !compact {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add(egui::Label::new(status_subline_text(&subline)).truncate());
                    });
                }
            });
        },
    );
}

/// 1px vertical hairline between bar groups.
fn vertical_divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, 32.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, Rounding::ZERO, Palette::BORDER);
}

// ---- Status strip layout ----

/// `Bardwave vX.Y.Z` (9px, tracked) with headroom for two-digit parts.
const STRIP_VERSION_W: f32 = 130.0;
/// Room kept for the `Update available: vX.Y.Z` link (it draws only when there
/// is an update).
const STRIP_UPDATE_W: f32 = 160.0;
/// Narrowest and widest the local-output readout gets.
const STRIP_READOUT_MIN_W: f32 = 100.0;
const STRIP_READOUT_MAX_W: f32 = 320.0;
/// Gap between the strip's groups.
const STRIP_GAP: f32 = Metrics::LG;

/// How much of the format text on the right the strip shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripFormat {
    /// `48 kHz / f32 / stereo  ·  N tracks  ·  M groups`.
    Full,
    /// `N tracks  ·  M groups`.
    Short,
    /// Nothing.
    Hidden,
}

impl StripFormat {
    /// Width reserved for the text (9px mono, tracked: about 6.4 per character,
    /// with room for four-digit counts).
    fn width(self) -> f32 {
        match self {
            Self::Full => 340.0,
            Self::Short => 150.0,
            Self::Hidden => 0.0,
        }
    }
}

/// How the strip spends its width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StripLayout {
    pub format: StripFormat,
    /// Width of the local-output readout.
    pub readout_w: f32,
}

impl StripLayout {
    /// Width of everything the strip reserves, padding and gaps included.
    pub fn width(&self) -> f32 {
        let format = if self.format == StripFormat::Hidden {
            0.0
        } else {
            STRIP_GAP + self.format.width()
        };
        Metrics::PANEL_PAD
            + STRIP_VERSION_W
            + STRIP_GAP
            + self.readout_w
            + STRIP_GAP
            + STRIP_UPDATE_W
            + format
            + Metrics::PANEL_PAD
    }
}

/// Layout of the strip for `width` points: the format text shortens and then
/// goes before the readout would drop under `STRIP_READOUT_MIN_W`; spare room
/// goes to the readout up to `STRIP_READOUT_MAX_W`.
pub fn strip_layout(width: f32) -> StripLayout {
    let mut chosen = StripLayout {
        format: StripFormat::Hidden,
        readout_w: STRIP_READOUT_MIN_W,
    };
    for format in [StripFormat::Full, StripFormat::Short] {
        let least = StripLayout {
            format,
            readout_w: STRIP_READOUT_MIN_W,
        };
        if least.width() <= width {
            chosen = least;
            break;
        }
    }
    let spare = width - chosen.width();
    chosen.readout_w =
        (STRIP_READOUT_MIN_W + spare).clamp(STRIP_READOUT_MIN_W, STRIP_READOUT_MAX_W);
    chosen
}

/// Bottom status strip (28px): version left, local output centre, format right.
pub fn status_strip(ui: &mut Ui, state: &SoundSharedApp) {
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, Rounding::ZERO, Palette::APP);
    let line = Rect::from_min_size(
        egui::pos2(rect.left(), rect.top()),
        Vec2::new(rect.width(), 1.0),
    );
    ui.painter()
        .rect_filled(line, Rounding::ZERO, Palette::BORDER);

    let layout = strip_layout(rect.width());
    let format_w = if layout.format == StripFormat::Hidden {
        0.0
    } else {
        layout.format.width()
    };
    // The format text is pinned to the right edge; the rest is clipped to what
    // is left of it, so nothing on the left can run under the text.
    let right_w = Metrics::PANEL_PAD + format_w;
    let left_rect = Rect::from_min_max(
        rect.min,
        egui::pos2(
            (rect.right() - right_w - if format_w > 0.0 { STRIP_GAP } else { 0.0 })
                .max(rect.left()),
            rect.bottom(),
        ),
    );
    let mut left = ui.new_child(
        UiBuilder::new()
            .max_rect(left_rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    left.set_clip_rect(left_rect.intersect(ui.clip_rect()));
    left.add_space(Metrics::PANEL_PAD);
    left.add(
        egui::Label::new(
            egui::RichText::new(format!("Bardwave v{}", crate::VERSION))
                .font(egui::FontId::new(9.0, egui::FontFamily::Monospace))
                .color(Palette::ZINC_600)
                .extra_letter_spacing(1.0),
        )
        .truncate(),
    );
    left.add_space(STRIP_GAP);
    local_audio_readout(&mut left, state, layout.readout_w);
    left.add_space(STRIP_GAP);
    crate::updates::status_indicator(&mut left, state);

    if format_w > 0.0 {
        let right_rect =
            Rect::from_min_max(egui::pos2(rect.right() - right_w, rect.top()), rect.max);
        let mut right = ui.new_child(
            UiBuilder::new()
                .max_rect(right_rect)
                .layout(Layout::right_to_left(Align::Center)),
        );
        right.add_space(Metrics::PANEL_PAD);
        let tracks = state.get_sounds().len();
        let groups = state.group_count();
        // The wire format is 32-bit float (see the browser client and the
        // broadcaster header), so the strip must not claim 16-bit.
        let label = match layout.format {
            StripFormat::Full => format!(
                "{} kHz / f32 / stereo  ·  {} tracks  ·  {} groups",
                crate::audio::decoder::TARGET_SAMPLE_RATE / 1000,
                tracks,
                groups
            ),
            _ => format!("{tracks} tracks  ·  {groups} groups"),
        };
        right.add(
            egui::Label::new(
                egui::RichText::new(label)
                    .font(egui::FontId::new(9.0, egui::FontFamily::Monospace))
                    .color(Palette::ZINC_600)
                    .extra_letter_spacing(1.0),
            )
            .truncate(),
        );
    }
}

/// Compact "where the sound is going" readout for the status strip.
///
/// The web listeners are fed from the mixer in every state, so a silent desktop
/// is otherwise indistinguishable from a working one: this strip is what says
/// `LOCAL OFF` (headless), `LOCAL FAILED` (with the device error) or the device
/// the mix is being played on.
fn local_audio_readout(ui: &mut Ui, state: &SoundSharedApp, max_w: f32) {
    use crate::audio::LocalOutputState as State;

    let status = state.local_output();
    let (dot, color, text) = match &status.state {
        State::Running => {
            let text = match status.device.as_deref() {
                Some(device) => format!("Local · {device}"),
                None => "Local".to_string(),
            };
            (Palette::ROSE_500, Palette::ZINC_400, text)
        }
        State::Headless => (
            Palette::AMBER_400,
            Palette::AMBER_400,
            "Local off — headless (network only)".to_string(),
        ),
        State::Failed(message) => (
            Palette::ROSE_700,
            Palette::ROSE_400,
            format!("Local failed — {message}"),
        ),
        State::Stopped => (
            Palette::ZINC_500,
            Palette::ZINC_600,
            "Local stopped".to_string(),
        ),
    };

    // The readout competes with the format text on the right, so it gets a
    // fixed width from the strip layout and is truncated inside it.
    ui.allocate_ui_with_layout(
        Vec2::new(max_w, ui.available_height()),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_max_width(max_w);
            crate::gui::widgets::signal_dot(
                ui,
                dot,
                6.0,
                matches!(status.state, State::Running) && !state.reduced_motion(),
            );
            ui.add_space(Metrics::SM);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(text.to_uppercase())
                        .font(egui::FontId::new(9.0, egui::FontFamily::Monospace))
                        .color(color)
                        .extra_letter_spacing(1.0),
                )
                .truncate(),
            );
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::scenes::contrast_ratio;

    fn rgb(c: egui::Color32) -> [u8; 3] {
        [c.r(), c.g(), c.b()]
    }

    #[test]
    fn nothing_running_is_idle() {
        assert_eq!(panic_state(BoardActivity::default()), PanicState::Idle);
    }

    #[test]
    fn anything_running_lights_the_button() {
        let cases = [
            BoardActivity {
                sounds: true,
                ..Default::default()
            },
            BoardActivity {
                scene: true,
                ..Default::default()
            },
            BoardActivity {
                music: true,
                ..Default::default()
            },
            BoardActivity {
                preview: true,
                ..Default::default()
            },
        ];
        for activity in cases {
            assert_eq!(panic_state(activity), PanicState::Live, "{activity:?}");
        }
    }

    #[test]
    fn live_and_idle_look_different() {
        for hovered in [false, true] {
            assert_ne!(
                panic_colors(PanicState::Live, hovered),
                panic_colors(PanicState::Idle, hovered)
            );
        }
        assert_ne!(
            panic_tooltip(PanicState::Live),
            panic_tooltip(PanicState::Idle)
        );
    }

    #[test]
    fn label_text_reads_in_every_state() {
        // Idle sits on the bar, the lit button on its own fill.
        for state in [PanicState::Idle, PanicState::Live] {
            for hovered in [false, true] {
                let colors = panic_colors(state, hovered);
                let background = if colors.fill == Palette::TRANSPARENT {
                    Palette::APP
                } else {
                    colors.fill
                };
                let ratio = contrast_ratio(rgb(colors.text), rgb(background));
                assert!(
                    ratio >= 4.5,
                    "{state:?} hovered={hovered}: contrast {ratio:.2}"
                );
            }
        }
    }

    /// Every integer width the console can be dragged to (and a little below the
    /// minimum window, in case a window manager ignores it).
    fn widths() -> impl Iterator<Item = f32> {
        (640..=1600).map(|w| w as f32)
    }

    #[test]
    fn the_plan_fits_every_width_down_to_a_tiny_window() {
        // Past the supported minimum the bar keeps shedding secondary things
        // (profile, master volume, lockup, status text) and still fits; what it
        // keeps is STOP ALL, REC, Help and the status dot.
        let floor = top_bar_layout(0.0);
        assert!(!floor.profile && !floor.master && floor.brand == BrandStyle::None);
        assert!(floor.status_dot, "{floor:?}");
        let needed = floor.total_width();
        assert!(needed <= 260.0, "the bar needs {needed} at its very least");
        for width in (needed.ceil() as u32..=1600).map(|w| w as f32) {
            let layout = top_bar_layout(width);
            assert!(layout.total_width() <= width, "{width}: {layout:?}");
        }
        // Below even that, the plan stays the floor (nothing panics, nothing
        // more is dropped): the drawing skips what has no room.
        assert_eq!(top_bar_layout(100.0), floor);
    }

    #[test]
    fn the_profile_never_takes_more_than_its_name_needs() {
        let mut widths = Widths::BUDGET;
        widths.profile_name = 40.0;
        let l = top_bar_layout_with(widths, 1600.0);
        assert_eq!(l.profile_text_w(), 40.0);
        let roomy = top_bar_layout(1600.0);
        assert_eq!(roomy.profile_text_w(), PROFILE_NAME_W);
        // A short name leaves more room for the rest in a tight window: the
        // bar keeps its captions where the full-width name would make it drop
        // them.
        let tight = top_bar_layout(1600.0).total_width() - 60.0;
        let short = top_bar_layout_with(widths, tight);
        let long = top_bar_layout(tight);
        assert!(short.total_width() <= tight && long.total_width() <= tight);
        assert!(short.gap >= long.gap && short.profile_name_w >= long.profile_name_w);
    }

    #[test]
    fn controls_and_status_never_overlap_at_any_width() {
        for width in widths() {
            let layout = top_bar_layout(width);
            assert!(
                layout.total_width() <= width,
                "{width}: needs {} for {layout:?}",
                layout.total_width()
            );
            let (left, right) = top_bar_split(&layout, width);
            // The controls end before the status cluster starts, with a gap.
            assert!(layout.left_width() <= left, "{width}: {layout:?}");
            assert!(
                left + SIDE_GAP + right <= width + 0.001,
                "{width}: split {left} + {right}"
            );
            assert!(right >= layout.right_width() - 0.001);
        }
    }

    #[test]
    fn the_bar_gives_up_things_in_the_agreed_order() {
        let full = top_bar_layout(1600.0);
        assert_eq!(
            full,
            TopBarLayout::full(Widths::BUDGET),
            "roomy bar loses nothing"
        );
        for width in widths() {
            let l = top_bar_layout(width);
            // Captions go before REC gets narrower.
            if l.rec_compact {
                assert!(!l.profile_label && !l.master_label, "{width}: {l:?}");
            }
            // The profile name shrinks before its caption goes.
            if !l.profile_label {
                assert!(l.profile_name_w < PROFILE_NAME_W, "{width}: {l:?}");
            }
            // The folder button is dropped only after REC went compact.
            if !l.rec_folder {
                assert!(l.rec_compact, "{width}: {l:?}");
            }
            assert!(l.profile_name_w >= PROFILE_NAME_MIN_W);
            assert!(l.master_slider_w >= 56.0);
        }
    }

    #[test]
    fn a_wider_window_never_gets_a_poorer_bar() {
        let mut previous = top_bar_layout(640.0);
        for width in widths().skip(1) {
            let l = top_bar_layout(width);
            assert!(l.profile_name_w >= previous.profile_name_w, "{width}");
            assert!(l.master_slider_w >= previous.master_slider_w, "{width}");
            assert!(l.gap >= previous.gap, "{width}");
            for (now, before, what) in [
                (l.profile_label, previous.profile_label, "profile label"),
                (l.master_label, previous.master_label, "master label"),
                (l.rec_folder, previous.rec_folder, "folder button"),
                (l.stop_all_label, previous.stop_all_label, "stop-all text"),
                (!l.rec_compact, !previous.rec_compact, "roomy REC"),
                (!l.status_compact, !previous.status_compact, "status line"),
            ] {
                assert!(now || !before, "{width}: {what} came back and left again");
            }
            previous = l;
        }
    }

    #[test]
    fn the_minimum_window_keeps_the_essentials() {
        let l = top_bar_layout(Metrics::MIN_WINDOW_W);
        // At 960 the captions are gone, but the controls keep their text and
        // the folder button; only the optional text is given up.
        assert_eq!(l.brand, BrandStyle::Compact);
        assert!(l.stop_all_label && l.rec_folder);
        assert!(l.total_width() <= Metrics::MIN_WINDOW_W);
    }

    /// Run the bar on its own at `width` and return its text boxes and the
    /// recorded extents.
    fn draw_bar_at(width: f32) -> (Vec<crate::gui::layout_probe::TextBox>, (f32, f32)) {
        use crate::gui::hotkeys_ui::test_support::harness;
        use crate::gui::layout_probe::{run_frames, text_boxes};
        let mut h = harness(crate::config::Config::default(), false);
        let ctx = h.ctx.clone();
        let out = run_frames(&ctx, egui::vec2(width, 200.0), 2, |ctx| {
            egui::TopBottomPanel::top("top_bar")
                .exact_height(Metrics::TOPBAR_H)
                .frame(egui::Frame::none())
                .show(ctx, |ui| top_bar(ui, &mut h.app));
        });
        let extent = ctx
            .data(|d| d.get_temp::<(f32, f32)>(egui::Id::new("top_bar_extent")))
            .expect("the bar records its extents under test");
        (text_boxes(&out), extent)
    }

    #[test]
    fn painted_bar_has_no_overlaps_at_any_width() {
        use crate::gui::layout_probe::assert_clean;
        for width in (640..=1600).step_by(8).chain([900, 960]) {
            let width = width as f32;
            let (boxes, (left_end, right_start)) = draw_bar_at(width);
            assert_clean(
                &boxes,
                Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(width, Metrics::TOPBAR_H)),
                &format!("top bar at {width}"),
            );
            assert!(
                left_end <= right_start,
                "{width}: the controls end at {left_end}, the status starts at {right_start}"
            );
            let status = boxes
                .iter()
                .find(|b| b.text == "OFF AIR")
                .unwrap_or_else(|| panic!("{width}: no OFF AIR status in {boxes:?}"));
            for b in boxes.iter().filter(|b| b.rect.left() < status.rect.left()) {
                assert!(
                    b.rect.right() <= status.rect.left() + 0.5,
                    "{width}: {:?} reaches the status",
                    b.text
                );
            }
        }
    }

    #[test]
    fn measured_items_fit_their_budgets() {
        let ctx = egui::Context::default();
        let measure = |text: &str, font: egui::FontId, spacing: f32| -> f32 {
            let mut width = 0.0;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                let job = egui::text::LayoutJob::single_section(
                    text.to_string(),
                    egui::TextFormat {
                        font_id: font.clone(),
                        extra_letter_spacing: spacing,
                        ..Default::default()
                    },
                );
                width = ctx.fonts(|f| f.layout_job(job).size().x);
            });
            width
        };
        let mono = crate::gui::theme::mono_font;
        let ui = crate::gui::theme::ui_font;
        let cases = [
            ("wordmark", measure("BARDWAVE", ui(18.0), -0.2), WORDMARK_W),
            (
                "caption",
                measure("BROADCAST CONSOLE", mono(9.0), 1.2),
                CAPTION_W,
            ),
            ("master", measure("MASTER", mono(10.0), 1.2), MASTER_LABEL_W),
            (
                "readout",
                measure("100%", mono(11.0), 0.0),
                MASTER_READOUT_W,
            ),
            (
                "profile",
                measure("PROFILE", mono(10.0), 1.2),
                PROFILE_LABEL_W,
            ),
            (
                "profile name",
                measure(&"W".repeat(18), mono(11.0), 0.0),
                PROFILE_NAME_W,
            ),
            (
                "help",
                measure("Help", mono(11.0), 0.0) + MENU_BTN_PAD,
                HELP_W,
            ),
            (
                "status line",
                measure("SERVER FAILED \u{2014} SEE PANEL", mono(9.0), 0.8),
                STATUS_FULL_W - 15.0,
            ),
            (
                "status headline",
                measure("STARTING", mono(11.0), 1.0),
                STATUS_COMPACT_W - 15.0,
            ),
        ];
        for (name, measured, budget) in cases {
            assert!(
                measured <= budget,
                "{name} measures {measured}, budget {budget}"
            );
        }
        // STOP ALL: pad + icon + gap + text + pad.
        let stop = 2.0 * Metrics::MD + 12.0 + Metrics::SM + measure("STOP ALL", mono(10.0), 0.0);
        assert!(stop <= STOP_ALL_W, "stop all measures {stop}");
    }

    #[test]
    fn strip_never_reserves_more_than_the_window_has() {
        for width in widths() {
            let l = strip_layout(width);
            assert!(l.width() <= width + 0.001, "{width}: {l:?}");
            assert!(l.readout_w >= STRIP_READOUT_MIN_W);
            assert!(l.readout_w <= STRIP_READOUT_MAX_W);
        }
        assert_eq!(strip_layout(1600.0).format, StripFormat::Full);
        assert_eq!(strip_layout(640.0).format, StripFormat::Short);
    }

    #[test]
    fn the_strip_version_comes_from_cargo() {
        // No literal version anywhere in the strip code: it formats
        // `crate::VERSION`, which is `env!("CARGO_PKG_VERSION")`.
        assert_eq!(crate::VERSION, env!("CARGO_PKG_VERSION"));
        let source = include_str!("top_bar.rs");
        let body = source.split("#[cfg(test)]\nmod tests").next().unwrap();
        assert!(body.contains("crate::VERSION"));
        assert!(!body.contains("format!(\"Bardwave v1"));
    }

    #[test]
    fn long_profile_names_are_cut_with_an_ellipsis_to_the_given_width() {
        let ctx = egui::Context::default();
        let font = crate::gui::widgets::glyph_font(11.0);
        let name = "A campaign with a very long profile name";
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let width_of = |t: &str| {
                    ui.fonts(|f| {
                        f.layout_no_wrap(t.to_string(), font.clone(), egui::Color32::WHITE)
                            .size()
                            .x
                    })
                };
                for max_w in [PROFILE_NAME_MIN_W, 56.0, 80.0, PROFILE_NAME_W] {
                    let shown = crate::gui::profiles_ui::fit_text(ui, name, font.clone(), max_w);
                    assert!(width_of(&shown) <= max_w, "{max_w}: {shown:?}");
                    assert!(shown.ends_with('\u{2026}'), "{max_w}: {shown:?}");
                    assert!(shown.chars().count() > 1, "{max_w}: some text stays");
                }
                // Short names are left alone.
                let short = crate::gui::profiles_ui::fit_text(ui, "Default", font.clone(), 80.0);
                assert_eq!(short, "Default");
            });
        });
    }

    #[test]
    fn spacing_tokens_keep_the_scale() {
        const { assert!(Metrics::LABEL_GAP >= Metrics::SM) };
        const { assert!(Metrics::BLOCK_GAP >= Metrics::MD) };
    }

    // ---- Painted-bar matrix: state x scale x width ----

    /// One state of the console the top bar is drawn in.
    #[derive(Debug, Clone)]
    struct BarScene {
        /// `(frames, bytes, dropped frames, warning)` of a running recording.
        recording: Option<(u64, u64, u64, Option<&'static str>)>,
        /// A feedback line (`text`, `is_error`) shown when nothing records.
        notice: Option<(&'static str, bool)>,
        server: ServerStatus,
        port: u16,
        /// `None` is the Default profile.
        profile: Option<&'static str>,
    }

    impl BarScene {
        fn idle() -> Self {
            Self {
                recording: None,
                notice: None,
                server: ServerStatus::Stopped,
                port: 8080,
                profile: None,
            }
        }
    }

    const SAMPLE_RATE: u64 = 48_000;
    const LONG_PROFILE: &str = "Campaign of the Seven Mountains 2";

    fn scenes() -> Vec<(&'static str, BarScene)> {
        let rec = |h: u64, bytes: u64, lost: u64, warn: Option<&'static str>| {
            Some((h * 3600 * SAMPLE_RATE, bytes, lost, warn))
        };
        let running = |s: BarScene| BarScene {
            server: ServerStatus::Running,
            ..s
        };
        vec![
            ("idle", BarScene::idle()),
            ("running :8080", running(BarScene::idle())),
            (
                "running :65535",
                BarScene {
                    port: 65535,
                    ..running(BarScene::idle())
                },
            ),
            (
                "starting",
                BarScene {
                    server: ServerStatus::Starting,
                    ..BarScene::idle()
                },
            ),
            (
                "error",
                BarScene {
                    server: ServerStatus::Error("port in use".into()),
                    ..BarScene::idle()
                },
            ),
            (
                "long profile",
                BarScene {
                    profile: Some(LONG_PROFILE),
                    ..BarScene::idle()
                },
            ),
            (
                "wide glyph profile",
                BarScene {
                    profile: Some("WWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWW"),
                    ..BarScene::idle()
                },
            ),
            (
                "cyrillic profile",
                BarScene {
                    profile: Some("Кампания в Тёмном лесу и в горах"),
                    ..BarScene::idle()
                },
            ),
            (
                "recording",
                BarScene {
                    recording: rec(1, 6_000_000_000, 0, None),
                    ..running(BarScene::idle())
                },
            ),
            (
                "recording, long profile, lost, warning",
                BarScene {
                    recording: rec(
                        10,
                        12_000_000_000,
                        1234,
                        Some("Disk space is running low (2.1 GB left)"),
                    ),
                    port: 65535,
                    profile: Some(LONG_PROFILE),
                    ..running(BarScene::idle())
                },
            ),
            (
                "recording, error server",
                BarScene {
                    recording: rec(0, 4_000_000, 0, None),
                    server: ServerStatus::Error("x".into()),
                    profile: Some("Camp"),
                    ..BarScene::idle()
                },
            ),
            (
                "saved notice",
                BarScene {
                    notice: Some((
                        "Saved bardwave-2026-11-19-203045.wav (00:12:34, 120.5 MB)",
                        false,
                    )),
                    ..running(BarScene::idle())
                },
            ),
            (
                "error notice, long profile",
                BarScene {
                    notice: Some((
                        "Cannot start recording: the recordings folder is read-only",
                        true,
                    )),
                    profile: Some(LONG_PROFILE),
                    ..BarScene::idle()
                },
            ),
        ]
    }

    /// Everything the matrix reads from one painted bar.
    struct PaintedBar {
        texts: Vec<crate::gui::layout_probe::TextBox>,
        widgets: Vec<Rect>,
        left_rect: Rect,
        right_rect: Rect,
        left_painted: Rect,
        right_painted: Rect,
    }

    fn draw_scene(scene: &BarScene, width_pt: f32, ppp: f32) -> PaintedBar {
        use crate::audio::recorder::{RecorderState, RecorderStats};
        use crate::gui::hotkeys_ui::test_support::harness;
        use crate::gui::layout_probe::{run_frames_at, text_boxes};
        let mut config = crate::config::Config::default();
        config.server.port = scene.port;
        let mut h = harness(config, false);
        let ctx = h.ctx.clone();
        crate::gui::install_theme(&ctx);
        h.app.profiles_ui.test_active = Some(scene.profile.map(str::to_string));
        h.app.server_state.lock().unwrap().status = scene.server.clone();
        let stats = scene
            .recording
            .map(|(frames, bytes, dropped, warning)| RecorderStats {
                state: RecorderState::Recording,
                frames_written: frames,
                bytes_written: bytes,
                dropped_frames: dropped,
                parts: 1,
                current_path: "/x/a.wav".into(),
                warning: warning.map(str::to_string),
                error: None,
            });
        let notice = scene
            .notice
            .map(|(text, is_error)| crate::gui::recorder_ui::Notice {
                text: text.to_string(),
                is_error,
            });
        h.app.recorder.fake_for_layout(stats, notice);

        let mut widgets = Vec::new();
        let out = run_frames_at(&ctx, egui::vec2(width_pt, 200.0), ppp, 3, |ctx| {
            widgets.clear();
            egui::TopBottomPanel::top("top_bar")
                .exact_height(Metrics::TOPBAR_H)
                .frame(egui::Frame::none())
                .show(ctx, |ui| top_bar(ui, &mut h.app));
            ctx.viewport(|vp| {
                for (_, list) in vp.this_pass.widgets.layers() {
                    widgets.extend(
                        list.iter()
                            .filter(|w| w.rect.top() < Metrics::TOPBAR_H)
                            .map(|w| w.interact_rect),
                    );
                }
            });
        });
        let [left_rect, right_rect, left_painted, right_painted] = ctx
            .data(|d| d.get_temp::<[Rect; 4]>(egui::Id::new("top_bar_rects")))
            .expect("the bar records its rects under test");
        PaintedBar {
            texts: text_boxes(&out)
                .into_iter()
                .filter(|t| t.rect.top() < Metrics::TOPBAR_H)
                .collect(),
            widgets,
            left_rect,
            right_rect,
            left_painted,
            right_painted,
        }
    }

    /// The right cluster's texts: Help and the status lines.
    fn is_right_text(text: &str) -> bool {
        matches!(
            text,
            "HELP" | "LIVE" | "OFF AIR" | "STARTING" | "ERROR" | "BINDING LISTENER"
        ) || text.starts_with("SERVER ")
    }

    /// Visible part of a painted text: the glyph box under its clip.
    fn visible(t: &crate::gui::layout_probe::TextBox) -> Rect {
        t.rect.intersect(t.clip)
    }

    fn touches(a: Rect, b: Rect) -> bool {
        const EPS: f32 = 0.5;
        let shared = a.intersect(b);
        shared.width() > EPS && shared.height() > EPS
    }

    /// What is wrong with the painted bar (empty when nothing).
    fn bar_problems(bar: &PaintedBar, width: f32) -> Vec<String> {
        const EPS: f32 = 0.5;
        let mut out = Vec::new();
        let right: Vec<_> = bar
            .texts
            .iter()
            .filter(|t| is_right_text(&t.text))
            .collect();
        let left: Vec<_> = bar
            .texts
            .iter()
            .filter(|t| !is_right_text(&t.text))
            .collect();
        if !right.iter().any(|t| t.text == "HELP") {
            out.push("Help is not painted".to_string());
        }
        // The menu buttons sit on the bar's centre line like the other
        // controls (Help used to hang several points below it).
        let mid = Metrics::TOPBAR_H / 2.0;
        for t in bar.texts.iter().filter(|t| t.text == "HELP") {
            if (t.rect.center().y - mid).abs() > 1.5 {
                out.push(format!(
                    "Help text centred at y {:.1}, the bar's centre line is {:.1}",
                    t.rect.center().y,
                    mid
                ));
            }
        }
        // The two clusters keep apart, as laid out and as painted.
        // The plan holds without help from the clip: what the controls took
        // lies inside their own rect.
        if bar.left_painted.right() > bar.left_rect.right() + EPS && bar.left_rect.width() > 0.0 {
            out.push(format!(
                "controls {:?} overflow their rect {:?}",
                bar.left_painted, bar.left_rect
            ));
        }
        if bar.left_painted.right() > bar.right_painted.left() + EPS {
            out.push(format!(
                "controls end at {:.1}, right cluster starts at {:.1}",
                bar.left_painted.right(),
                bar.right_painted.left()
            ));
        }
        if bar.right_painted.left() < bar.right_rect.left() - EPS {
            out.push(format!(
                "right cluster {:?} spills out of its rect {:?}",
                bar.right_painted, bar.right_rect
            ));
        }
        if bar.left_rect.right() > bar.right_rect.left() + EPS {
            out.push(format!(
                "rects overlap: {:?} / {:?}",
                bar.left_rect, bar.right_rect
            ));
        }
        // Help and the status stay whole inside their rect and clear of the
        // controls' texts.
        for r in &right {
            if r.rect.left() < bar.right_rect.left() - EPS
                || r.rect.right() > bar.right_rect.right() + EPS
            {
                out.push(format!(
                    "{:?} {:?} leaves the right rect {:?}",
                    r.text, r.rect, bar.right_rect
                ));
            }
            for l in &left {
                if touches(visible(r), visible(l)) {
                    out.push(format!(
                        "{:?} {:?} is painted over {:?} {:?}",
                        r.text, r.rect, l.text, l.rect
                    ));
                }
            }
        }
        // Controls: the Help button's box is clear of every other control.
        let help = bar
            .texts
            .iter()
            .find(|t| t.text == "HELP")
            .map(|t| t.rect.center());
        if let Some(c) = help {
            let button = bar
                .widgets
                .iter()
                .filter(|w| w.contains(c))
                .min_by(|a, b| a.area().total_cmp(&b.area()));
            if let Some(button) = button {
                for w in bar.widgets.iter().filter(|w| *w != button) {
                    if touches(*button, *w)
                        && !w.contains_rect(*button)
                        && !button.contains_rect(*w)
                    {
                        out.push(format!("Help button {button:?} overlaps control {w:?}"));
                    }
                }
            }
        }
        // No two texts overlap, and everything is on the bar.
        for (i, a) in bar.texts.iter().enumerate() {
            for b in &bar.texts[i + 1..] {
                if touches(visible(a), visible(b)) {
                    out.push(format!(
                        "{:?} {:?} overlaps {:?} {:?}",
                        a.text, a.rect, b.text, b.rect
                    ));
                }
            }
        }
        // In the supported range nothing is cut off either.
        if width >= Metrics::MIN_WINDOW_W {
            for t in &bar.texts {
                if t.rect.left() < t.clip.left() - EPS || t.rect.right() > t.clip.right() + EPS {
                    out.push(format!(
                        "{:?} {:?} is cut by its clip {:?}",
                        t.text, t.rect, t.clip
                    ));
                }
            }
        }
        out
    }

    /// Interface scales: 0.8 to 1.6 on a 1x screen, and the same on a Retina one.
    const SCALES: [f32; 7] = [0.8, 1.0, 1.3, 1.6, 2.0, 2.6, 3.2];

    /// Windows from the minimum up to 1600 points.
    fn supported_widths() -> Vec<f32> {
        let mut v: Vec<f32> = (960..=1600).step_by(80).map(|w| w as f32).collect();
        v.extend([961.0, 1003.0, 1111.0]);
        v
    }

    fn matrix_lines(widths: &[f32], scales: &[f32]) -> Vec<String> {
        let mut lines = Vec::new();
        for (name, scene) in scenes() {
            for &ppp in scales {
                for &width in widths {
                    let bar = draw_scene(&scene, width, ppp);
                    for p in bar_problems(&bar, width) {
                        lines.push(format!("[{name}] {width} pt @{ppp}: {p}"));
                    }
                }
            }
        }
        for l in &lines {
            println!("{l}");
        }
        lines
    }

    #[test]
    fn painted_bar_matrix_has_no_overlap_in_supported_windows() {
        let lines = matrix_lines(&supported_widths(), &SCALES);
        assert!(
            lines.is_empty(),
            "{} bar problems (listed above)",
            lines.len()
        );
    }

    #[test]
    fn painted_bar_matrix_never_overlaps_below_the_minimum_either() {
        // A window can be narrower than the minimum in points: 960 px at a
        // 160 % scale is 600 pt, and a window manager may ignore the minimum.
        let widths: Vec<f32> = (240..960).step_by(16).map(|w| w as f32).collect();
        let lines = matrix_lines(&widths, &[0.8, 1.0, 1.6, 2.0]);
        assert!(
            lines.is_empty(),
            "{} bar problems (listed above)",
            lines.len()
        );
    }
}

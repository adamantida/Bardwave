//! Custom-painted console widgets for the Red Noir broadcast console.
//!
//! egui's default widgets are soft and rounded; the console needs hairline
//! structure, square geometry, mono telemetry and a single rose accent. These
//! helpers paint that look directly and report interaction through `Response`.

use std::borrow::Cow;

use egui::{Color32, CursorIcon, FontId, Rect, Response, Rounding, Sense, Stroke, Ui, Vec2};

use crate::gui::theme::{alpha, mono_font, Metrics, Palette};

/// A small vector icon, painted with strokes and shapes.
///
/// The console used to draw its icons as Unicode glyphs (⧉ ⟳ ⟲ ✎ ≋ …) in the
/// monospace font. egui's bundled fonts have none of those code points, so
/// most buttons showed an empty "tofu" box instead of an icon. Painting them
/// needs no icon font, scales with the button and always renders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Play,
    Stop,
    Cross,
    Edit,
    Copy,
    Plus,
    Check,
    Refresh,
    Loop,
    Wave,
    Trash,
    Volume,
    Muted,
    Broadcast,
    Globe,
    Folder,
}

/// Kept as an alias so the call sites read the same as before.
pub type Glyph = Icon;

pub mod glyph {
    use super::{Glyph, Icon};

    pub const PLAY: Glyph = Icon::Play;
    pub const STOP: Glyph = Icon::Stop;
    pub const CROSS: Glyph = Icon::Cross;
    pub const EDIT: Glyph = Icon::Edit;
    pub const COPY: Glyph = Icon::Copy;
    pub const PLUS: Glyph = Icon::Plus;
    pub const CHECK: Glyph = Icon::Check;
    pub const REFRESH: Glyph = Icon::Refresh;
    pub const LOOP: Glyph = Icon::Loop;
    pub const WAVE: Glyph = Icon::Wave;
    pub const TRASH: Glyph = Icon::Trash;
    pub const VOLUME: Glyph = Icon::Volume;
    pub const MUTED: Glyph = Icon::Muted;
    pub const BROADCAST: Glyph = Icon::Broadcast;
    pub const GLOBE: Glyph = Icon::Globe;
    pub const FOLDER: Glyph = Icon::Folder;
}

/// Paint `icon` centred on `center`, `size` pixels tall.
pub fn paint_icon(
    painter: &egui::Painter,
    icon: Icon,
    center: egui::Pos2,
    size: f32,
    color: Color32,
) {
    use egui::{pos2, Shape};
    use std::f32::consts::PI;

    let s = size;
    let p = |x: f32, y: f32| pos2(center.x + x * s, center.y + y * s);
    let width = (s * 0.11).max(1.2);
    let stroke = Stroke::new(width, color);
    let line = |points: Vec<egui::Pos2>| Shape::line(points, stroke);
    let arc = |cx: f32, cy: f32, r: f32, from: f32, to: f32| -> Vec<egui::Pos2> {
        let steps = 20;
        (0..=steps)
            .map(|i| {
                let a = from + (to - from) * i as f32 / steps as f32;
                p(cx + r * a.cos(), cy + r * a.sin())
            })
            .collect()
    };
    // Open arrowhead at `tip`, pointing along `angle` (radians).
    let arrow = |tip: egui::Pos2, angle: f32| {
        let len = s * 0.2;
        let a1 = angle + PI * 0.8;
        let a2 = angle - PI * 0.8;
        Shape::line(
            vec![
                pos2(tip.x + len * a1.cos(), tip.y + len * a1.sin()),
                tip,
                pos2(tip.x + len * a2.cos(), tip.y + len * a2.sin()),
            ],
            stroke,
        )
    };

    let shapes: Vec<Shape> = match icon {
        Icon::Play => vec![Shape::convex_polygon(
            vec![p(-0.3, -0.4), p(0.4, 0.0), p(-0.3, 0.4)],
            color,
            Stroke::NONE,
        )],
        Icon::Stop => vec![Shape::rect_filled(
            Rect::from_center_size(center, Vec2::splat(s * 0.64)),
            Rounding::same(s * 0.06),
            color,
        )],
        Icon::Cross => vec![
            line(vec![p(-0.32, -0.32), p(0.32, 0.32)]),
            line(vec![p(-0.32, 0.32), p(0.32, -0.32)]),
        ],
        Icon::Plus => vec![
            line(vec![p(-0.4, 0.0), p(0.4, 0.0)]),
            line(vec![p(0.0, -0.4), p(0.0, 0.4)]),
        ],
        Icon::Check => vec![line(vec![p(-0.38, 0.02), p(-0.1, 0.3), p(0.4, -0.3)])],
        Icon::Edit => vec![
            // Pencil body as an outlined slanted bar, plus the tip.
            Shape::closed_line(
                vec![p(0.2, -0.42), p(0.42, -0.2), p(-0.18, 0.4), p(-0.4, 0.18)],
                stroke,
            ),
            line(vec![p(-0.4, 0.18), p(-0.45, 0.45), p(-0.18, 0.4)]),
        ],
        Icon::Copy => vec![
            // Back sheet (only the visible L), front sheet outlined.
            line(vec![p(-0.42, 0.16), p(-0.42, -0.42), p(0.16, -0.42)]),
            Shape::rect_stroke(
                Rect::from_min_max(p(-0.2, -0.2), p(0.42, 0.42)),
                Rounding::same(s * 0.06),
                stroke,
            ),
        ],
        Icon::Refresh => {
            let start = -PI * 0.35;
            let end = PI * 1.35;
            let r = 0.36;
            vec![
                line(arc(0.0, 0.0, r, start, end)),
                arrow(p(r * end.cos(), r * end.sin()), end + PI / 2.0),
            ]
        }
        Icon::Loop => vec![
            line(vec![p(-0.42, 0.12), p(-0.42, -0.22), p(0.36, -0.22)]),
            arrow(p(0.36, -0.22), 0.0),
            line(vec![p(0.42, -0.12), p(0.42, 0.22), p(-0.36, 0.22)]),
            arrow(p(-0.36, 0.22), PI),
        ],
        Icon::Wave => {
            let points = (0..=24)
                .map(|i| {
                    let t = i as f32 / 24.0;
                    p(-0.42 + 0.84 * t, -0.28 * (t * 2.0 * PI * 1.5).sin())
                })
                .collect();
            vec![line(points)]
        }
        Icon::Trash => vec![
            line(vec![p(-0.42, -0.26), p(0.42, -0.26)]),
            line(vec![
                p(-0.14, -0.26),
                p(-0.12, -0.42),
                p(0.12, -0.42),
                p(0.14, -0.26),
            ]),
            Shape::closed_line(
                vec![p(-0.3, -0.18), p(0.3, -0.18), p(0.24, 0.42), p(-0.24, 0.42)],
                stroke,
            ),
            line(vec![p(-0.06, -0.04), p(-0.06, 0.28)]),
            line(vec![p(0.08, -0.04), p(0.08, 0.28)]),
        ],
        Icon::Volume | Icon::Muted => {
            let mut shapes = vec![Shape::convex_polygon(
                vec![
                    p(-0.45, -0.14),
                    p(-0.24, -0.14),
                    p(0.0, -0.38),
                    p(0.0, 0.38),
                    p(-0.24, 0.14),
                    p(-0.45, 0.14),
                ],
                color,
                Stroke::NONE,
            )];
            if icon == Icon::Muted {
                shapes.push(line(vec![p(0.14, -0.16), p(0.44, 0.16)]));
                shapes.push(line(vec![p(0.14, 0.16), p(0.44, -0.16)]));
            } else {
                shapes.push(line(arc(0.0, 0.0, 0.22, -PI * 0.3, PI * 0.3)));
                shapes.push(line(arc(0.0, 0.0, 0.4, -PI * 0.3, PI * 0.3)));
            }
            shapes
        }
        Icon::Broadcast => vec![
            Shape::circle_filled(center, s * 0.1, color),
            line(arc(0.0, 0.0, 0.26, -PI * 0.25, PI * 0.25)),
            line(arc(0.0, 0.0, 0.26, PI * 0.75, PI * 1.25)),
            line(arc(0.0, 0.0, 0.44, -PI * 0.25, PI * 0.25)),
            line(arc(0.0, 0.0, 0.44, PI * 0.75, PI * 1.25)),
        ],
        Icon::Globe => vec![
            Shape::circle_stroke(center, s * 0.42, stroke),
            line(vec![p(-0.42, 0.0), p(0.42, 0.0)]),
            Shape::ellipse_stroke(center, Vec2::new(s * 0.18, s * 0.42), stroke),
        ],
        Icon::Folder => vec![Shape::closed_line(
            vec![
                p(-0.45, -0.3),
                p(-0.12, -0.3),
                p(-0.02, -0.18),
                p(0.45, -0.18),
                p(0.45, 0.36),
                p(-0.45, 0.36),
            ],
            stroke,
        )],
    };
    painter.extend(shapes);
}

/// Visual weight of a button.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BtnKind {
    /// Solid rose-600 fill, white label. Primary action.
    Primary,
    /// Transparent fill, hairline border, zinc-300 label.
    Ghost,
    /// Transparent fill, rose-700 hairline, rose-400 label.
    Danger,
    /// Solid rose-950 fill, rose-400 label (used while a sound is playing).
    Armed,
}

/// A console button with optional leading glyph, square 2px geometry.
pub struct ConsoleButton<'a> {
    text: Cow<'a, str>,
    glyph: Option<Glyph>,
    kind: BtnKind,
    size: Vec2,
    enabled: bool,
    tooltip: Option<Cow<'a, str>>,
    id_salt: Option<&'a str>,
    font_size: f32,
    tracking: f32,
}

impl<'a> ConsoleButton<'a> {
    pub fn new(text: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: text.into(),
            glyph: None,
            kind: BtnKind::Ghost,
            size: Vec2::new(0.0, Metrics::BTN_H),
            enabled: true,
            tooltip: None,
            id_salt: None,
            font_size: 11.0,
            tracking: 0.9,
        }
    }

    pub fn primary(text: impl Into<Cow<'a, str>>) -> Self {
        Self::new(text).kind(BtnKind::Primary)
    }

    pub fn ghost(text: impl Into<Cow<'a, str>>) -> Self {
        Self::new(text).kind(BtnKind::Ghost)
    }

    pub fn danger(text: impl Into<Cow<'a, str>>) -> Self {
        Self::new(text).kind(BtnKind::Danger)
    }

    pub fn kind(mut self, kind: BtnKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn glyph(mut self, glyph: Glyph) -> Self {
        self.glyph = Some(glyph);
        self
    }

    /// Explicit size. A zero width means "fill the available width".
    pub fn size(mut self, size: Vec2) -> Self {
        self.size = size;
        self
    }

    pub fn height(mut self, height: f32) -> Self {
        self.size.y = height;
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        self.size.x = width;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<Cow<'a, str>>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    pub fn id_salt(mut self, salt: &'a str) -> Self {
        self.id_salt = Some(salt);
        self
    }

    pub fn small(mut self) -> Self {
        self.size.y = Metrics::BTN_H_SM;
        self.font_size = 10.0;
        self
    }

    pub fn font_size(mut self, size: f32) -> Self {
        self.font_size = size;
        self
    }

    pub fn no_tracking(mut self) -> Self {
        self.tracking = 0.0;
        self
    }

    /// Paint the button and return its response.
    pub fn show(self, ui: &mut Ui) -> Response {
        // A stable id makes hover/tooltip state survive frames even though the
        // button paints itself instead of using `ui.button`.
        let salt = match self.id_salt {
            Some(salt) => salt.to_string(),
            None => self.text.to_string(),
        };
        ui.push_id(("console_btn", salt), |ui| self.show_inner(ui))
            .inner
    }

    fn show_inner(self, ui: &mut Ui) -> Response {
        let width = if self.size.x > 0.0 {
            self.size.x
        } else {
            ui.available_width().max(40.0)
        };
        let size = Vec2::new(width, self.size.y);

        let sense = if self.enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, mut resp) = ui.allocate_exact_size(size, sense);
        if self.enabled {
            resp = resp.on_hover_cursor(CursorIcon::PointingHand);
        }

        let hovered = self.enabled && resp.hovered();
        let pressed = self.enabled && resp.is_pointer_button_down_on();

        // Keyboard accessibility, both halves of it:
        //
        // * egui reports `Space`/`Enter` on a focused widget as
        //   `fake_primary_click`, so it is folded into `clicked` here and every
        //   caller keeps using `.clicked()`;
        // * clicking a focusable widget does not move focus by itself, so a
        //   click focuses it — which is what makes the ring appear and the
        //   follow-up keystroke work.
        if resp.clicked() {
            resp.request_focus();
        }
        if resp.fake_primary_click {
            resp.clicked = true;
        }
        let focused = self.enabled && resp.has_focus();

        let (fill, border, text_color, glyph_color) = match self.kind {
            BtnKind::Primary => {
                // Pressed and hovered both darken to rose-700.
                let fill = if pressed || hovered {
                    Palette::ROSE_700
                } else {
                    Palette::ROSE_600
                };
                (fill, Palette::ROSE_600, Palette::WHITE, Palette::WHITE)
            }
            BtnKind::Ghost => {
                let fill = if pressed {
                    Palette::BORDER
                } else if hovered {
                    Palette::ROW
                } else {
                    Palette::TRANSPARENT
                };
                let border = if hovered {
                    Palette::BORDER_STRONG
                } else {
                    Palette::BORDER
                };
                (fill, border, Palette::ZINC_300, Palette::ZINC_400)
            }
            BtnKind::Danger => {
                let fill = if pressed || hovered {
                    alpha(Palette::ROSE_950, 180)
                } else {
                    Palette::TRANSPARENT
                };
                (
                    fill,
                    Palette::ROSE_700,
                    Palette::ROSE_400,
                    Palette::ROSE_400,
                )
            }
            BtnKind::Armed => {
                let fill = if hovered {
                    alpha(Palette::ROSE_950, 230)
                } else {
                    Palette::ROSE_950
                };
                let border = if hovered {
                    Palette::ROSE_500
                } else {
                    alpha(Palette::ROSE_500, 150)
                };
                (fill, border, Palette::ROSE_400, Palette::ROSE_400)
            }
        };

        let fill = if self.enabled {
            fill
        } else {
            Palette::TRANSPARENT
        };
        let text_color = if self.enabled {
            text_color
        } else {
            alpha(Palette::ZINC_600, 140)
        };

        let painter = ui.painter();
        if fill != Palette::TRANSPARENT {
            painter.rect_filled(rect, Rounding::same(Metrics::R_BTN), fill);
        }
        painter.rect_stroke(
            rect,
            Rounding::same(Metrics::R_BTN),
            Stroke::new(
                1.0_f32,
                if self.enabled {
                    border
                } else {
                    Palette::BORDER
                },
            ),
        );

        // 1px rose-400 focus ring, drawn just outside the border so it never
        // hides the button's own edge.
        if focused {
            painter.rect_stroke(
                rect.expand(1.0),
                Rounding::same(Metrics::R_BTN + 1.0),
                Stroke::new(1.0_f32, Palette::ROSE_400),
            );
        }

        let font = mono_font(self.font_size);
        let label = self.text.to_uppercase();

        // Measure the full label block (icon + gap + text), then centre it.
        let text_galley = painter.layout_no_wrap(label.clone(), font.clone(), text_color);
        let icon_size = (self.font_size * 1.25).round();
        let has_text = !label.is_empty();
        let icon_w = match self.glyph {
            Some(_) if has_text => icon_size + 8.0,
            Some(_) => icon_size,
            None => 0.0,
        };
        let total_w = icon_w + if has_text { text_galley.size().x } else { 0.0 };
        let mut cursor_x = rect.center().x - total_w / 2.0;

        if let Some(icon) = self.glyph {
            let icon_color = if self.enabled {
                glyph_color
            } else {
                alpha(Palette::ZINC_600, 140)
            };
            paint_icon(
                painter,
                icon,
                egui::pos2(cursor_x + icon_size / 2.0, rect.center().y),
                icon_size,
                icon_color,
            );
            cursor_x += icon_w;
        }
        if has_text {
            let pos = egui::pos2(cursor_x, rect.center().y - text_galley.size().y / 2.0);
            painter.galley(pos, text_galley, text_color);
        }

        if let Some(tooltip) = self.tooltip {
            resp = resp.on_hover_text(tooltip.as_ref());
        }
        resp
    }
}

/// Width of a [`bar_menu_button`] showing `text`: the text and the horizontal
/// padding of a console button, never narrower than egui's minimum target.
pub fn bar_menu_width(ui: &Ui, text: &str) -> f32 {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), glyph_font(11.0), Palette::ZINC_300);
    (galley.size().x + 2.0 * Metrics::MD)
        .max(ui.spacing().interact_size.x)
        .ceil()
}

/// A menu opened from the top bar: painted like the ghost console buttons next
/// to it (same height, border, hover) instead of egui's grey menu button, and
/// centred on the line it is placed on. The popup opens below the button and
/// closes when something in it is clicked or the user clicks elsewhere.
///
/// `text` is shown as typed (no upper-casing: profile names are the user's).
pub fn bar_menu_button<R>(
    ui: &mut Ui,
    id_salt: &str,
    text: &str,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> Response {
    let width = bar_menu_width(ui, text);
    let popup_id = ui.make_persistent_id(("bar_menu_popup", id_salt));
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, Metrics::BTN_H_SM), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    if resp.clicked() {
        resp.request_focus();
        ui.memory_mut(|m| m.toggle_popup(popup_id));
    }
    // Space / Enter on the focused button opens it, like a click.
    if resp.fake_primary_click && !resp.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup_id));
    }

    let open = ui.memory(|m| m.is_popup_open(popup_id));
    let hovered = resp.hovered();
    let pressed = resp.is_pointer_button_down_on();
    let fill = if pressed {
        Palette::BORDER
    } else if hovered || open {
        Palette::ROW
    } else {
        Palette::TRANSPARENT
    };
    let border = if hovered || open {
        Palette::BORDER_STRONG
    } else {
        Palette::BORDER
    };
    let painter = ui.painter();
    if fill != Palette::TRANSPARENT {
        painter.rect_filled(rect, Rounding::same(Metrics::R_BTN), fill);
    }
    painter.rect_stroke(
        rect,
        Rounding::same(Metrics::R_BTN),
        Stroke::new(1.0_f32, border),
    );
    if resp.has_focus() {
        painter.rect_stroke(
            rect.expand(1.0),
            Rounding::same(Metrics::R_BTN + 1.0),
            Stroke::new(1.0_f32, Palette::ROSE_400),
        );
    }
    let galley = painter.layout_no_wrap(text.to_string(), glyph_font(11.0), Palette::ZINC_300);
    let pos = egui::pos2(
        rect.center().x - galley.size().x / 2.0,
        rect.center().y - galley.size().y / 2.0,
    );
    painter.galley(pos, galley, Palette::ZINC_300);

    egui::popup_below_widget(
        ui,
        popup_id,
        &resp,
        egui::PopupCloseBehavior::CloseOnClick,
        add_contents,
    );
    resp
}

/// Square icon button used for transport controls and row actions.
pub struct IconButton<'a> {
    glyph: Glyph,
    kind: BtnKind,
    size: f32,
    tooltip: Option<Cow<'a, str>>,
    id_salt: Option<&'a str>,
    font_size: f32,
}

impl<'a> IconButton<'a> {
    pub fn new(glyph: Glyph) -> Self {
        Self {
            glyph,
            kind: BtnKind::Ghost,
            size: Metrics::TRANSPORT,
            tooltip: None,
            id_salt: None,
            font_size: 12.0,
        }
    }

    pub fn kind(mut self, kind: BtnKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<Cow<'a, str>>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    pub fn id_salt(mut self, salt: &'a str) -> Self {
        self.id_salt = Some(salt);
        self
    }

    pub fn font_size(mut self, size: f32) -> Self {
        self.font_size = size;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let salt = match self.id_salt {
            Some(salt) => salt.to_string(),
            None => format!("{:?}", self.glyph),
        };
        ui.push_id(("console_icon", salt), |ui| self.show_inner(ui))
            .inner
    }

    fn show_inner(self, ui: &mut Ui) -> Response {
        let size = Vec2::splat(self.size);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let mut resp = resp.on_hover_cursor(CursorIcon::PointingHand);
        let hovered = resp.hovered();
        let pressed = resp.is_pointer_button_down_on();

        // Same keyboard contract as `ConsoleButton`: a click focuses the button,
        // and `Space`/`Enter` on the focused button counts as a click.
        if resp.clicked() {
            resp.request_focus();
        }
        if resp.fake_primary_click {
            resp.clicked = true;
        }
        let focused = resp.has_focus();

        let (fill, border, fg) = match self.kind {
            BtnKind::Primary => {
                let fill = if hovered || pressed {
                    Palette::ROSE_700
                } else {
                    Palette::ROSE_600
                };
                (fill, fill, Palette::WHITE)
            }
            BtnKind::Armed => {
                let fill = if hovered {
                    alpha(Palette::ROSE_950, 235)
                } else {
                    Palette::ROSE_950
                };
                let border = if hovered {
                    Palette::ROSE_500
                } else {
                    alpha(Palette::ROSE_500, 150)
                };
                (fill, border, Palette::ROSE_400)
            }
            BtnKind::Danger => {
                let fill = if hovered {
                    alpha(Palette::ROSE_950, 180)
                } else {
                    Palette::TRANSPARENT
                };
                (fill, Palette::ROSE_700, Palette::ROSE_400)
            }
            BtnKind::Ghost => {
                let fill = if hovered {
                    alpha(Palette::BORDER, 160)
                } else {
                    Palette::TRANSPARENT
                };
                (
                    fill,
                    if hovered {
                        alpha(Palette::BORDER_STRONG, 120)
                    } else {
                        Palette::TRANSPARENT
                    },
                    if hovered {
                        Palette::WHITE
                    } else {
                        Palette::ZINC_500
                    },
                )
            }
        };

        let painter = ui.painter();
        if fill != Palette::TRANSPARENT {
            painter.rect_filled(rect, Rounding::same(Metrics::R_SM), fill);
        }
        if border != Palette::TRANSPARENT {
            painter.rect_stroke(
                rect,
                Rounding::same(Metrics::R_SM),
                Stroke::new(1.0_f32, border),
            );
        }
        if focused {
            painter.rect_stroke(
                rect.expand(1.0),
                Rounding::same(Metrics::R_SM + 1.0),
                Stroke::new(1.0_f32, Palette::ROSE_400),
            );
        }
        paint_icon(
            painter,
            self.glyph,
            rect.center(),
            (self.font_size * 1.25).round(),
            fg,
        );

        match self.tooltip {
            Some(tooltip) => resp.on_hover_text(tooltip.as_ref()),
            None => resp,
        }
    }
}

/// Horizontal console slider: 2px track, filled rose portion, square 9px thumb.
///
/// Returns the response; `changed()` tells whether the value was modified.
pub fn level_slider(ui: &mut Ui, value: &mut f32, width: f32, id_salt: &str) -> Response {
    let height = 18.0;
    let (rect, mut resp) =
        ui.allocate_exact_size(Vec2::new(width, height), Sense::click_and_drag());

    let min_y = rect.center().y - 1.0;
    let track = Rect::from_min_size(egui::pos2(rect.left(), min_y), Vec2::new(rect.width(), 2.0));

    let mut value_changed = false;
    if let Some(pointer) = resp.interact_pointer_pos() {
        if resp.is_pointer_button_down_on() || resp.dragged() {
            let t = ((pointer.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
            if (t - *value).abs() > f32::EPSILON {
                *value = t;
                value_changed = true;
            }
        }
    }
    // Keyboard adjustment for accessibility.
    if resp.has_focus() {
        let step = if ui.input(|i| i.modifiers.shift) {
            0.1
        } else {
            0.02
        };
        let delta = ui.input(|i| {
            let mut d = 0.0;
            if i.key_pressed(egui::Key::ArrowRight) || i.key_pressed(egui::Key::ArrowUp) {
                d += step;
            }
            if i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::ArrowDown) {
                d -= step;
            }
            d
        });
        if delta != 0.0 {
            *value = (*value + delta).clamp(0.0, 1.0);
            value_changed = true;
        }
    }
    if value_changed {
        resp.mark_changed();
    }
    resp = resp.on_hover_cursor(CursorIcon::ResizeHorizontal);
    resp.widget_info(|| {
        egui::WidgetInfo::slider(true, *value as f64, format!("{:.0}%", *value * 100.0))
    });

    let hovered = resp.hovered() || resp.dragged();
    let value = value.clamp(0.0, 1.0);

    let painter = ui.painter();
    painter.rect_filled(track, Rounding::ZERO, Palette::BORDER_STRONG);
    if value > 0.0 {
        let filled =
            Rect::from_min_size(track.min, Vec2::new(track.width() * value, track.height()));
        painter.rect_filled(filled, Rounding::ZERO, Palette::ROSE_500);
    }
    let thumb_x = track.left() + track.width() * value;
    let thumb_size = if hovered { 6.0 } else { 4.5 };
    let thumb = Rect::from_center_size(
        egui::pos2(thumb_x, track.center().y),
        Vec2::splat(thumb_size),
    );
    painter.rect_filled(thumb, Rounding::ZERO, Palette::ROSE_500);

    let _ = id_salt;
    resp
}

/// A segmented meter (e.g. the 20-cell master output strip).
pub fn segmented_meter(ui: &mut Ui, value: f32, cells: usize, width: f32, height: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    if cells == 0 {
        return;
    }
    let gap = 3.0;
    let cell_w = ((rect.width() - gap * (cells as f32 - 1.0)) / cells as f32).max(1.0);
    let lit = (value.clamp(0.0, 1.0) * cells as f32).round() as usize;
    let painter = ui.painter();
    for i in 0..cells {
        let x = rect.left() + (cell_w + gap) * i as f32;
        let cell = Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(cell_w, rect.height()));
        let color = if i < lit {
            Palette::ROSE_500
        } else {
            Palette::BORDER
        };
        painter.rect_filled(cell, Rounding::ZERO, color);
    }
}

/// Small inventory of a series of square cells (listener occupancy).
pub fn occupancy_cells(ui: &mut Ui, filled: usize, total: usize, cell: f32, gap: f32) {
    let width = total as f32 * cell + gap * (total.saturating_sub(1)) as f32;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, cell), Sense::hover());
    let painter = ui.painter();
    for i in 0..total {
        let x = rect.left() + (cell + gap) * i as f32;
        let r = Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::splat(cell));
        let color = if i < filled {
            Palette::ROSE_500
        } else {
            Palette::BORDER
        };
        painter.rect_filled(r, Rounding::ZERO, color);
    }
}

/// Signal dot. `pulse` animates the opacity in a slow 2s breath.
pub fn signal_dot(ui: &mut Ui, color: Color32, diameter: f32, pulse: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::hover());
    let color = if pulse {
        let t = ui.input(|i| i.time) as f32;
        let phase = ((t * std::f32::consts::PI).sin() + 1.0) * 0.5; // 0..1, 2s period
        let a = (120.0 + 135.0 * phase) as u8;
        alpha(color, a)
    } else {
        color
    };
    ui.painter()
        .circle_filled(rect.center(), diameter / 2.0, color);
    resp
}

/// Shorten a string to fit roughly `max_chars` characters, adding an ellipsis.
pub fn elide(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let keep = max_chars.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    out.push('\u{2026}');
    out
}

/// Format seconds as `MM:SS`.
pub fn format_duration(seconds: f32) -> String {
    let total = seconds.max(0.0).round() as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

/// Format a sample rate as `48 KHZ`.
pub fn format_sample_rate(hz: u32) -> String {
    if hz >= 1000 && hz.is_multiple_of(1000) {
        format!("{} kHz", hz / 1000)
    } else {
        format!("{:.1} kHz", hz as f32 / 1000.0)
    }
}

/// Uppercase file extension of a path, or `AUDIO` when unknown.
pub fn format_extension(path: &std::path::Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_uppercase())
        .unwrap_or_else(|| "AUDIO".to_string())
}

/// Monospace font used for painted glyphs at a given size.
pub fn glyph_font(size: f32) -> FontId {
    mono_font(size)
}

//! SoundShared — "Red Noir Broadcast Console" design tokens.
//!
//! Mirrors `.superdesign/design-system.md` and the Red Noir console mockup:
//! near-black surfaces, hairline zinc structure, a single hot rose accent used
//! like a signal lamp, monospaced telemetry, and square-ish 2–4px geometry.
//!
//! Everything in the GUI should read its colors, sizes and type from here so
//! the whole app stays on one palette.

use egui::{
    Color32, FontFamily, FontId, Frame, Margin, Rounding, Shadow, Stroke, Style, TextStyle,
};

/// Red Noir color palette — the only colors the GUI may use.
pub struct Palette;

impl Palette {
    // ----- Surfaces -----
    /// Window / page background.
    pub const APP: Color32 = Color32::from_rgb(0x0A, 0x0A, 0x0A);
    /// Primary panel surface.
    pub const PANEL: Color32 = Color32::from_rgb(0x11, 0x11, 0x11);
    /// Deepest inset (wells, fields, meters).
    pub const WELL: Color32 = Color32::from_rgb(0x09, 0x09, 0x0B);
    /// Elevated surface / row background.
    pub const ROW: Color32 = Color32::from_rgb(0x18, 0x18, 0x1B);
    /// Row background on hover.
    pub const ROW_HOVER: Color32 = Color32::from_rgb(0x1F, 0x1F, 0x23);

    // ----- Hairlines -----
    /// Default border / divider.
    pub const BORDER: Color32 = Color32::from_rgb(0x27, 0x27, 0x2A);
    /// Strong border / slider track.
    pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x3F, 0x3F, 0x46);

    // ----- Zinc text ramp -----
    pub const ZINC_600: Color32 = Color32::from_rgb(0x52, 0x52, 0x5B);
    pub const ZINC_500: Color32 = Color32::from_rgb(0x71, 0x71, 0x7A);
    pub const ZINC_400: Color32 = Color32::from_rgb(0xA1, 0xA1, 0xAA);
    pub const ZINC_300: Color32 = Color32::from_rgb(0xD4, 0xD4, 0xD8);
    pub const WHITE: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
    pub const BLACK: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
    pub const TRANSPARENT: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 0);

    // ----- Rose accent ramp -----
    /// Clipping accent / playing text.
    pub const ROSE_400: Color32 = Color32::from_rgb(0xFB, 0x71, 0x85);
    /// Live / playing signal.
    pub const ROSE_500: Color32 = Color32::from_rgb(0xF4, 0x3F, 0x5E);
    /// Primary brand accent (primary actions).
    pub const ROSE_600: Color32 = Color32::from_rgb(0xE1, 0x1D, 0x48);
    /// Primary hover / pressed.
    pub const ROSE_700: Color32 = Color32::from_rgb(0xBE, 0x12, 0x3C);
    /// Meter base / tinted inset.
    pub const ROSE_950: Color32 = Color32::from_rgb(0x4C, 0x05, 0x19);
    /// Starting / buffer warning.
    pub const AMBER_400: Color32 = Color32::from_rgb(0xFB, 0xBF, 0x24);

    // ----- Semantic aliases (kept for readability at call sites) -----
    pub const TEXT_PRIMARY: Color32 = Self::WHITE;
    pub const TEXT_SECONDARY: Color32 = Self::ZINC_400;
    pub const TEXT_MUTED: Color32 = Self::ZINC_500;
    pub const TEXT_DIM: Color32 = Self::ZINC_600;
    pub const BG_PRIMARY: Color32 = Self::APP;
    pub const BG_SECONDARY: Color32 = Self::PANEL;
    pub const BG_TERTIARY: Color32 = Self::ROW;
    pub const BG_INPUT: Color32 = Self::WELL;
    pub const BORDER_PRIMARY: Color32 = Self::BORDER;
    pub const BORDER_FOCUS: Color32 = Self::ROSE_600;
    pub const PRIMARY: Color32 = Self::ROSE_600;
    pub const PRIMARY_HOVER: Color32 = Self::ROSE_700;
    pub const PRIMARY_LIGHT: Color32 = Self::ROSE_400;
    pub const ACCENT: Color32 = Self::ROSE_600;
    pub const SUCCESS: Color32 = Self::ROSE_500;
    pub const DANGER: Color32 = Self::ROSE_700;
    pub const WARNING: Color32 = Self::AMBER_400;
    pub const STATUS_STOPPED: Color32 = Self::ZINC_500;
    pub const STATUS_STARTING: Color32 = Self::AMBER_400;
    pub const STATUS_RUNNING: Color32 = Self::ROSE_500;
    pub const STATUS_ERROR: Color32 = Self::ROSE_400;
}

/// Layout metrics for the console.
pub struct Metrics;

impl Metrics {
    // Spacing scale
    pub const XS: f32 = 4.0;
    pub const SM: f32 = 8.0;
    pub const MD: f32 = 12.0;
    pub const LG: f32 = 16.0;
    pub const XL: f32 = 24.0;
    pub const XXL: f32 = 32.0;

    /// Panel inner padding.
    pub const PANEL_PAD: f32 = 24.0;
    /// Outer window padding around the main area.
    pub const APP_PAD: f32 = 24.0;
    /// Top command bar height.
    pub const TOPBAR_H: f32 = 64.0;
    /// Bottom status strip height.
    pub const STATUS_H: f32 = 28.0;
    /// Fixed width of the network panel column.
    pub const RIGHT_COL_W: f32 = 340.0;
    /// Gap between the two main columns.
    pub const COL_GAP: f32 = 24.0;
    /// Sound row height.
    pub const ROW_H: f32 = 64.0;
    /// Gap between sound rows.
    pub const ROW_GAP: f32 = 10.0;
    /// Smallest gap between a caption and the control it names (panels zero
    /// egui's implicit spacing, so every such gap is placed by hand).
    pub const LABEL_GAP: f32 = Self::SM;
    /// Smallest gap between two blocks of a panel.
    pub const BLOCK_GAP: f32 = Self::MD;
    /// Gap between the lines of a two-line lockup (title over caption).
    pub const LOCKUP_GAP: f32 = 2.0;

    // Radii — nothing above 4.
    pub const R_PANEL: f32 = 4.0;
    pub const R_SM: f32 = 2.0;
    pub const R_BTN: f32 = 2.0;

    // Component sizes
    pub const BTN_H: f32 = 36.0;
    pub const BTN_H_SM: f32 = 28.0;
    pub const TRANSPORT: f32 = 32.0;
    pub const ROW_SLIDER_W: f32 = 80.0;
    pub const MASTER_SLIDER_W: f32 = 150.0;
    pub const MICRO: f32 = 10.0;
    pub const MICRO_TINY: f32 = 9.0;

    // Group rail — the vertical category list inside the library panel.
    /// Width of the group rail column.
    pub const RAIL_W: f32 = 148.0;
    /// Height of one rail entry.
    pub const RAIL_ITEM_H: f32 = 30.0;
    /// Narrowest a sound row may get before the rail collapses. The rail is the
    /// primary navigation, so it is only hidden when rows would be unreadable.
    pub const MIN_ROW_W: f32 = 240.0;
    /// Panel radius for the group rail chips.
    pub const R_RAIL: f32 = 2.0;
}

/// Font families used by the console.
pub fn mono_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

pub fn ui_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Uppercase micro-label: 10px mono, tracked out, zinc-500.
pub fn micro_label(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into().to_uppercase())
        .font(mono_font(Metrics::MICRO))
        .color(Palette::ZINC_500)
        .extra_letter_spacing(1.2)
}

/// Rose micro-label used for section codes such as `01 / LIBRARY`.
pub fn micro_label_accent(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into().to_uppercase())
        .font(mono_font(Metrics::MICRO))
        .color(Palette::ROSE_500)
        .extra_letter_spacing(1.2)
}

/// Dim micro-label (footer copy).
pub fn micro_label_dim(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into().to_uppercase())
        .font(mono_font(Metrics::MICRO_TINY))
        .color(Palette::ZINC_600)
        .extra_letter_spacing(1.0)
}

/// Panel title: 18px, uppercase, tight tracking.
pub fn panel_title(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into().to_uppercase())
        .font(ui_font(18.0))
        .color(Palette::WHITE)
        .strong()
        .extra_letter_spacing(0.4)
}

/// Brand wordmark: 18px, uppercase, black weight.
pub fn wordmark(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into().to_uppercase())
        .font(ui_font(18.0))
        .color(Palette::WHITE)
        .strong()
        .extra_letter_spacing(-0.2)
}

/// Monospaced telemetry text.
pub fn telemetry(text: impl Into<String>, size: f32, color: Color32) -> egui::RichText {
    egui::RichText::new(text.into())
        .font(mono_font(size))
        .color(color)
}

/// Tracked uppercase mono text (for buttons and chips).
pub fn tracked(
    text: impl Into<String>,
    size: f32,
    color: Color32,
    tracking: f32,
) -> egui::RichText {
    egui::RichText::new(text.into().to_uppercase())
        .font(mono_font(size))
        .color(color)
        .extra_letter_spacing(tracking)
}

/// Body / name text.
pub fn name_text(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into())
        .font(ui_font(14.0))
        .color(Palette::WHITE)
}

/// Secondary mono line under a row name.
pub fn row_subline(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into().to_uppercase())
        .font(mono_font(Metrics::MICRO))
        .color(Palette::ZINC_500)
        .extra_letter_spacing(0.6)
}

/// Translucent variant of a palette color.
pub fn alpha(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

/// Build the egui style for the Red Noir console.
pub fn create_theme() -> Style {
    let mut style = Style::default();
    let visuals = &mut style.visuals;

    visuals.dark_mode = true;

    // Surfaces
    visuals.window_fill = Palette::PANEL;
    visuals.panel_fill = Palette::APP;
    visuals.faint_bg_color = Palette::ROW;
    visuals.extreme_bg_color = Palette::WELL;
    visuals.window_rounding = Rounding::same(Metrics::R_PANEL);
    visuals.menu_rounding = Rounding::same(Metrics::R_SM);

    // Widgets — flat, hairline structure, no soft pills.
    visuals.widgets.noninteractive.bg_fill = Palette::PANEL;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, Palette::BORDER);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, Palette::ZINC_300);
    visuals.widgets.noninteractive.rounding = Rounding::same(Metrics::R_SM);

    visuals.widgets.inactive.bg_fill = Palette::ROW;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Palette::BORDER);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, Palette::ZINC_300);
    visuals.widgets.inactive.rounding = Rounding::same(Metrics::R_SM);

    visuals.widgets.hovered.bg_fill = Palette::ROW_HOVER;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Palette::BORDER_STRONG);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, Palette::WHITE);
    visuals.widgets.hovered.rounding = Rounding::same(Metrics::R_SM);

    visuals.widgets.active.bg_fill = Palette::ROSE_700;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, Palette::ROSE_600);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, Palette::WHITE);
    visuals.widgets.active.rounding = Rounding::same(Metrics::R_SM);

    visuals.widgets.open.bg_fill = Palette::ROW;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, Palette::BORDER_STRONG);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0_f32, Palette::WHITE);
    visuals.widgets.open.rounding = Rounding::same(Metrics::R_SM);

    // Selection — rose tint over a hairline.
    visuals.selection.bg_fill = alpha(Palette::ROSE_600, 40);
    visuals.selection.stroke = Stroke::new(1.0_f32, Palette::ROSE_500);

    visuals.hyperlink_color = Palette::ROSE_500;
    visuals.override_text_color = Some(Palette::ZINC_300);

    // Window chrome
    visuals.window_stroke = Stroke::new(1.0_f32, Palette::BORDER);
    visuals.window_shadow = Shadow {
        offset: egui::vec2(0.0, 8.0),
        blur: 24.0,
        spread: 0.0,
        color: Color32::from_black_alpha(160),
    };
    visuals.popup_shadow = Shadow {
        offset: egui::vec2(0.0, 6.0),
        blur: 18.0,
        spread: 0.0,
        color: Color32::from_black_alpha(140),
    };
    visuals.resize_corner_size = 12.0;

    // Type scale
    style.text_styles = [
        (TextStyle::Heading, ui_font(18.0)),
        (TextStyle::Body, ui_font(13.0)),
        (TextStyle::Monospace, mono_font(11.0)),
        (TextStyle::Button, mono_font(11.0)),
        (TextStyle::Small, mono_font(10.0)),
    ]
    .into();

    // Spacing
    style.spacing.item_spacing = egui::vec2(Metrics::SM, Metrics::SM);
    style.spacing.button_padding = egui::vec2(Metrics::MD, 0.0);
    style.spacing.window_margin = Margin::same(0.0);
    style.spacing.menu_margin = Margin::same(Metrics::SM);
    style.spacing.slider_width = Metrics::ROW_SLIDER_W;
    style.spacing.interact_size = egui::vec2(24.0, 24.0);
    // Thin floating scroll bars in the panel colours: they appear on hover
    // and never steal layout width, so a panel that overflows by a few
    // pixels no longer shifts its content sideways. The default handle used
    // the (light grey) text colour, which glared against the noir panels.
    style.spacing.scroll = egui::style::ScrollStyle::thin();
    style.spacing.scroll.foreground_color = false;

    style.interaction.selectable_labels = false;
    style.interaction.tooltip_delay = 0.35;
    style.animation_time = 0.15;

    style
}

/// Standard console panel: `#111111` fill, 1px hairline, 4px radius, 24px pad.
pub fn panel_frame() -> Frame {
    Frame::none()
        .fill(Palette::PANEL)
        .stroke(Stroke::new(1.0_f32, Palette::BORDER))
        .rounding(Rounding::same(Metrics::R_PANEL))
        .inner_margin(Margin::same(Metrics::PANEL_PAD))
}

/// Draw a panel with the standard console frame.
pub fn card_panel<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    panel_frame().show(ui, add_contents).inner
}

/// A console panel pinned to an exact size.
///
/// `egui` shrinks a `Frame` run to its content; the console needs the two main
/// columns to fill the window instead, so the panel is painted into a rect that
/// the shell sized and the content is laid out inside the panel padding.
pub fn console_panel<R>(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect(
        rect,
        Rounding::same(Metrics::R_PANEL),
        Palette::PANEL,
        Stroke::new(1.0_f32, Palette::BORDER),
    );

    let inner_rect = rect.shrink(Metrics::PANEL_PAD);
    let mut inner = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );

    // Panels place every gap explicitly, so egui's implicit `item_spacing.y`
    // between widgets is pure overhead — about 150px of it across a panel, which
    // is what pushed the lower controls out of the window. Zero it so the column
    // height is exactly what the layout asks for and the panel fits.
    inner.spacing_mut().item_spacing.y = 0.0;

    // The columns are fixed-size, so a short window would otherwise paint the
    // lower controls past the panel edge. Scrolling keeps them reachable and
    // clips everything to the panel body; with the spacing above it stays hidden
    // at the default window size.
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(&mut inner, |ui| add_contents(ui))
        .inner
}

/// Full-width 1px hairline divider.
pub fn hairline(ui: &mut egui::Ui) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 1.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, Rounding::ZERO, Palette::BORDER);
}

/// Vertical hairline (used in the top command bar).
pub fn vertical_rule(ui: &mut egui::Ui, height: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, height), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, Rounding::ZERO, Palette::BORDER);
}

/// Panel header: `01 / LIBRARY` code above an uppercase title, optional right action.
pub fn panel_header(
    ui: &mut egui::Ui,
    code: &str,
    title: &str,
    action: Option<impl FnOnce(&mut egui::Ui)>,
) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(micro_label_accent(code));
            ui.add_space(2.0);
            ui.label(panel_title(title));
        });
        if let Some(action_fn) = action {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), action_fn);
        }
    });
}

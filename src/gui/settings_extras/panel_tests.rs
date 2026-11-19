//! Tests of the panel polish: interface scale, empty states, scene warning
//! badges and dialogs that must fit the minimum window.

use std::time::{Duration, Instant};

use egui::{vec2, Rect};

use super::*;
use crate::config::{
    clamp_ui_scale, Config, SceneConfig, SceneItemConfig, UI_SCALE_MAX, UI_SCALE_MIN,
};
use crate::gui::hotkeys_ui::test_support::{harness, painted_text, Harness};
use crate::gui::scenes::{
    scene_warning_lines, short_warning, warning_tooltip, WarningCache, WARNING_REFRESH,
};
use crate::gui::theme::Metrics;

const SCENE: &str = "22222222-2222-4222-8222-222222222222";
const GHOST_SOUND: &str = "33333333-3333-4333-8333-333333333333";

/// One frame on a `size` screen whose monitor has `native` pixels per point.
fn frame_at(
    ctx: &egui::Context,
    size: egui::Vec2,
    native: f32,
    mut run: impl FnMut(&egui::Context),
) -> egui::FullOutput {
    let mut info = egui::ViewportInfo {
        native_pixels_per_point: Some(native),
        ..Default::default()
    };
    info.inner_rect = Some(Rect::from_min_size(egui::Pos2::ZERO, size));
    let mut input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
        ..Default::default()
    };
    input.viewports.insert(egui::ViewportId::ROOT, info);
    ctx.run(input, &mut run)
}

fn config_with_ghost_scene() -> Config {
    let mut config = Config::default();
    config.library.scenes.push(SceneConfig {
        id: SCENE.to_string(),
        name: "Tavern".to_string(),
        items: vec![SceneItemConfig {
            sound: GHOST_SOUND.to_string(),
            volume: 0.5,
            looped: true,
            start: Default::default(),
            delay_ms: 0,
            pan: 0.0,
        }],
        ..Default::default()
    });
    config
}

fn draw_deck(h: &mut Harness, size: egui::Vec2) -> String {
    let ctx = h.ctx.clone();
    let mut text = String::new();
    for _ in 0..3 {
        let out = frame_at(&ctx, size, 1.0, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                crate::gui::scenes::scenes_deck(ui, &mut h.app);
            });
        });
        text = painted_text(&out);
    }
    text
}

// ---- interface scale ----

#[test]
fn slider_positions_round_trip_on_five_percent_steps() {
    assert_eq!(scale_to_position(UI_SCALE_MIN), 0.0);
    assert_eq!(scale_to_position(UI_SCALE_MAX), 1.0);
    let mut seen = 0;
    let mut scale = UI_SCALE_MIN;
    while scale <= UI_SCALE_MAX + 1e-4 {
        let back = position_to_scale(scale_to_position(scale));
        assert!((back - scale).abs() < 1e-4, "{scale} came back as {back}");
        seen += 1;
        scale += UI_SCALE_STEP;
    }
    assert_eq!(seen, 17, "0.80 .. 1.60 in 5 % steps");
    assert_eq!(position_to_scale(scale_to_position(1.0)), 1.0);
}

#[test]
fn slider_positions_snap_and_stay_in_range() {
    assert_eq!(position_to_scale(-3.0), UI_SCALE_MIN);
    assert_eq!(position_to_scale(7.0), UI_SCALE_MAX);
    let between = position_to_scale(0.37);
    let steps = between / UI_SCALE_STEP;
    assert!(
        (steps - steps.round()).abs() < 1e-3,
        "{between} is not on a step"
    );
}

#[test]
fn scale_text_and_clamping() {
    assert_eq!(format_scale(1.0), "100%");
    assert_eq!(format_scale(1.3), "130%");
    assert_eq!(format_scale(9.0), "160%");
    assert_eq!(clamp_ui_scale(0.1), UI_SCALE_MIN);
    assert_eq!(clamp_ui_scale(f32::NAN), 1.0);
    assert_eq!(clamp_ui_scale(f32::INFINITY), 1.0);
}

#[test]
fn the_scale_multiplies_the_monitors_own_ratio() {
    assert_eq!(target_pixels_per_point(Some(2.0), 1.3), 2.6);
    assert_eq!(target_pixels_per_point(None, 1.0), 1.0);
    assert_eq!(target_pixels_per_point(Some(0.0), 1.2), 1.2);
    assert_eq!(target_pixels_per_point(Some(f32::NAN), 1.2), 1.2);
    assert_eq!(target_pixels_per_point(Some(2.0), 5.0), 2.0 * UI_SCALE_MAX);
}

#[test]
fn a_too_small_window_is_grown_to_the_layout_minimum() {
    assert_eq!(grown_window(vec2(1280.0, 800.0), MIN_WINDOW), None);
    assert_eq!(grown_window(MIN_WINDOW, MIN_WINDOW), None);
    assert_eq!(
        grown_window(vec2(738.0, 477.0), MIN_WINDOW),
        Some(MIN_WINDOW)
    );
    assert_eq!(
        grown_window(vec2(1500.0, 400.0), MIN_WINDOW),
        Some(vec2(1500.0, 620.0))
    );
}

#[test]
fn applying_the_scale_changes_pixels_per_point_without_a_restart() {
    let ctx = egui::Context::default();
    let size = vec2(1280.0, 800.0);
    for _ in 0..3 {
        frame_at(&ctx, size, 2.0, |ctx| apply_ui_scale(ctx, 1.3));
    }
    assert!((ctx.pixels_per_point() - 2.6).abs() < 1e-3);
    for _ in 0..3 {
        frame_at(&ctx, size, 2.0, |ctx| apply_ui_scale(ctx, 1.0));
    }
    assert!((ctx.pixels_per_point() - 2.0).abs() < 1e-3);
    // Keyboard zoom would fight the slider.
    assert!(!ctx.options(|o| o.zoom_with_keyboard));
}

#[test]
fn a_new_scale_restates_the_window_minimum_and_grows_a_small_window() {
    let ctx = egui::Context::default();
    // 960x620 physical-ish window at 1.3: the layout needs 960x620 points.
    let small = vec2(738.0, 477.0);
    let mut commands = Vec::new();
    for _ in 0..4 {
        let out = frame_at(&ctx, small, 1.0, |ctx| apply_ui_scale(ctx, 1.3));
        if let Some(viewport) = out.viewport_output.get(&egui::ViewportId::ROOT) {
            commands.extend(viewport.commands.iter().cloned());
        }
    }
    assert!(
        commands
            .iter()
            .any(|c| matches!(c, egui::ViewportCommand::MinInnerSize(s) if *s == MIN_WINDOW)),
        "{commands:?}"
    );
    assert!(
        commands
            .iter()
            .any(|c| matches!(c, egui::ViewportCommand::InnerSize(s) if *s == MIN_WINDOW)),
        "{commands:?}"
    );
}

#[test]
fn the_interface_slider_saves_the_scale_when_the_pointer_is_up() {
    let mut h = harness(Config::default(), false);
    assert_eq!(h.config.lock().unwrap().gui.scale(), 1.0);
    h.app.extras.mix.scale_edit = Some(1.3);
    let ctx = h.ctx.clone();
    let mut text = String::new();
    for _ in 0..2 {
        let out = frame_at(&ctx, vec2(960.0, 1600.0), 1.0, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| mix_section(ui, &mut h.app));
        });
        text = painted_text(&out);
    }
    assert!(text.contains("INTERFACE"), "{text}");
    assert!(text.contains("SCALE"), "{text}");
    assert!(text.contains("130%"), "{text}");
    let saved = h.config.lock().unwrap().gui.ui_scale;
    assert!((saved - 1.3).abs() < 1e-4, "saved {saved}");
    assert!(h.app.library_dirty, "the config is queued for saving");
    assert_eq!(h.app.extras.mix.scale_edit, None);
}

#[test]
fn the_scene_frame_applies_the_configured_scale() {
    let mut config = Config::default();
    config.gui.ui_scale = 1.5;
    let mut h = harness(config, false);
    let ctx = h.ctx.clone();
    for _ in 0..3 {
        frame_at(&ctx, vec2(1280.0, 800.0), 1.0, |ctx| h.app.scene_frame(ctx));
    }
    assert!((ctx.pixels_per_point() - 1.5).abs() < 1e-3);
}

// ---- empty states ----

#[test]
fn an_empty_scene_deck_invites_the_first_scene() {
    let mut h = harness(Config::default(), false);
    let text = draw_deck(&mut h, vec2(900.0, 600.0));
    assert!(text.contains("NO SCENES YET"), "{text}");
    assert!(text.contains("CREATE YOUR FIRST SCENE"), "{text}");
    assert!(text.contains("SAVE MIX AS SCENE"), "{text}");
    assert!(text.contains("NEW SCENE"), "{text}");
}

#[test]
fn timers_hotkeys_and_playlists_say_what_to_do_when_empty() {
    use crate::gui::dialog::show_dialogs;
    let mut h = harness(Config::default(), false);
    let ctx = h.ctx.clone();
    let size = vec2(960.0, 620.0);

    h.app.open_timers();
    let mut text = String::new();
    for _ in 0..3 {
        text = painted_text(&frame_at(&ctx, size, 1.0, |ctx| {
            show_dialogs(ctx, &mut h.app)
        }));
    }
    assert!(text.contains("NO TIMERS YET"), "{text}");
    assert!(text.contains("NEW TIMER"), "{text}");

    h.app.open_hotkeys();
    for _ in 0..3 {
        text = painted_text(&frame_at(&ctx, size, 1.0, |ctx| {
            show_dialogs(ctx, &mut h.app)
        }));
    }
    assert!(text.contains("NO HOTKEYS YET"), "{text}");
    assert!(text.contains("CAPTURE KEY"), "{text}");

    h.app.open_music();
    for _ in 0..3 {
        text = painted_text(&frame_at(&ctx, size, 1.0, |ctx| {
            show_dialogs(ctx, &mut h.app)
        }));
    }
    assert!(text.contains("NO PLAYLISTS YET"), "{text}");
    assert!(text.contains("CREATE A PLAYLIST"), "{text}");
}

// ---- scene warnings ----

#[test]
fn warning_lines_drop_the_scene_name_and_start_with_a_capital() {
    assert_eq!(
        short_warning(
            "scene \u{201c}Tavern\u{201d} is empty, activating it starts nothing",
            "Tavern"
        ),
        "Is empty, activating it starts nothing"
    );
    assert_eq!(short_warning("other text", "Tavern"), "Other text");
    assert_eq!(short_warning("", "Tavern"), "");
}

#[test]
fn the_tooltip_lists_problems_and_counts_the_rest() {
    let few = vec!["One".to_string(), "Two".to_string()];
    assert_eq!(warning_tooltip(&few), "\u{2022} One\n\u{2022} Two");
    let many: Vec<String> = (1..=9).map(|n| format!("P{n}")).collect();
    let text = warning_tooltip(&many);
    assert!(text.contains("\u{2022} P6"));
    assert!(!text.contains("P7"));
    assert!(text.ends_with("and 3 more"), "{text}");
}

#[test]
fn warning_lines_cover_only_scenes_with_problems() {
    let h = harness(config_with_ghost_scene(), false);
    let lines = scene_warning_lines(h.app.library(), |_| true);
    let id: crate::gui::groups::SceneId = SCENE.parse().unwrap();
    let found = lines.get(&id).expect("the ghost sound is flagged");
    assert!(!found.is_empty());
    assert!(
        found.iter().all(|line| !line.starts_with("scene ")),
        "{found:?}"
    );

    let clean = harness(Config::default(), false);
    assert!(scene_warning_lines(clean.app.library(), |_| true).is_empty());
}

#[test]
fn the_cache_refreshes_off_thread_and_on_a_schedule() {
    let h = harness(config_with_ghost_scene(), false);
    let id: crate::gui::groups::SceneId = SCENE.parse().unwrap();
    let mut cache = WarningCache::default();
    let t0 = Instant::now();
    assert!(cache.due(t0), "the first lookup is due at once");
    assert!(cache.lines(id).is_empty());

    cache.pump(h.app.library(), t0, || {});
    assert!(!cache.due(t0), "a lookup is already running");
    let deadline = Instant::now() + Duration::from_secs(5);
    while cache.lines(id).is_empty() && Instant::now() < deadline {
        cache.pump(h.app.library(), Instant::now(), || {});
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!cache.lines(id).is_empty(), "the worker delivered");

    // Fresh result: nothing is due until the refresh interval passes ...
    let now = Instant::now();
    cache.pump(h.app.library(), now, || {});
    assert!(!cache.due(now));
    assert!(cache.due(now + WARNING_REFRESH + Duration::from_millis(1)));
    // ... or the library changes.
    cache.invalidate();
    assert!(cache.due(now));
}

#[test]
fn a_scene_card_with_a_problem_draws_its_badge() {
    let mut h = harness(config_with_ghost_scene(), false);
    let id: crate::gui::groups::SceneId = SCENE.parse().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while h.app.scenes.warnings.lines(id).is_empty() && Instant::now() < deadline {
        h.app.scene_frame(&h.ctx.clone());
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!h.app.scenes.warnings.lines(id).is_empty());
    let text = draw_deck(&mut h, vec2(900.0, 600.0));
    assert!(text.contains("TAVERN"), "{text}");
    assert!(text.contains('!'), "the badge is painted: {text}");
}

// ---- fit at the minimum window ----

#[test]
fn every_modal_fits_the_960_by_620_window() {
    use crate::gui::dialog::show_dialogs;
    let size = vec2(960.0, 620.0);
    let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
    let mut h = harness(config_with_ghost_scene(), false);
    let ctx = h.ctx.clone();

    // (area id, opener, content width the dialog asks for). A row of buttons
    // without a width fills the row and pushes the window wider than its
    // content; the width check catches that.
    type Open = fn(&mut crate::gui::app::SoundSharedApp);
    let frame_pad = 2.0 * Metrics::PANEL_PAD;
    let cases: [(&str, Open, f32); 5] = [
        ("hotkeys_modal", |app| app.open_hotkeys(), 560.0),
        ("timers_modal", |app| app.open_timers(), 560.0),
        ("music_manager", |app| app.open_music(), 780.0),
        ("scene_editor_modal", |app| app.open_new_scene(), 690.0),
        (
            "scene_editor_modal",
            |app| {
                let id: crate::gui::groups::SceneId = SCENE.parse().unwrap();
                app.open_edit_scene(id);
            },
            690.0,
        ),
    ];
    for (area, open, content_w) in cases {
        open(&mut h.app);
        for _ in 0..4 {
            frame_at(&ctx, size, 1.0, |ctx| show_dialogs(ctx, &mut h.app));
        }
        let rect = ctx
            .memory(|m| m.area_rect(egui::Id::new(area)))
            .unwrap_or_else(|| panic!("{area} is open"));
        assert!(
            screen.expand(1.0).contains_rect(rect),
            "{area} sticks out of a 960x620 window: {rect:?}"
        );
        assert!(
            (rect.width() - (content_w + frame_pad)).abs() < 2.0,
            "{area} is {} wide, its content asks for {}",
            rect.width(),
            content_w + frame_pad
        );
        h.app.close_dialog();
    }
}

#[test]
fn the_deck_and_the_network_column_lay_out_at_the_minimum_size() {
    let mut h = harness(config_with_ghost_scene(), false);
    let ctx = h.ctx.clone();
    for _ in 0..3 {
        frame_at(&ctx, vec2(960.0, 620.0), 1.0, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                crate::gui::scenes::scenes_deck(ui, &mut h.app);
            });
        });
        frame_at(&ctx, vec2(960.0, 620.0), 1.0, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                crate::gui::server_panel::server_panel(ui, &mut h.app, vec2(340.0, 560.0));
            });
        });
    }
}

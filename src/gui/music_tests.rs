//! Unit tests of `gui::music` (included from there).

use super::*;
use std::path::Path;

fn state(status: PlaylistStatus) -> PlaylistState {
    PlaylistState {
        status,
        ..Default::default()
    }
}

fn library_with_sounds(n: usize) -> (Library, Vec<SoundId>) {
    let mut library = Library::new();
    for i in 0..n {
        let path = format!("/tmp/bardwave-test-{i}.ogg");
        assert!(library.insert_sound(Path::new(&path), &format!("Sound {i}"), 1.0, None));
    }
    let ids = library.sounds().iter().map(|s| s.id).collect();
    (library, ids)
}

// ------------------------------------------------------------ time ----

#[test]
fn clock_formats_minutes_and_hours() {
    assert_eq!(format_clock(0), "0:00");
    assert_eq!(format_clock(999), "0:00");
    assert_eq!(format_clock(65_000), "1:05");
    assert_eq!(format_clock(3_599_000), "59:59");
    assert_eq!(format_clock(3_725_000), "1:02:05");
}

#[test]
fn progress_is_clamped_and_zero_without_a_length() {
    assert_eq!(progress_fraction(5_000, Some(10_000)), 0.5);
    assert_eq!(progress_fraction(20_000, Some(10_000)), 1.0);
    assert_eq!(progress_fraction(5_000, None), 0.0);
    assert_eq!(progress_fraction(5_000, Some(0)), 0.0);
}

#[test]
fn pointer_fraction_stays_on_the_bar() {
    assert_eq!(fraction_from_x(150.0, 100.0, 200.0), 0.25);
    assert_eq!(fraction_from_x(50.0, 100.0, 200.0), 0.0);
    assert_eq!(fraction_from_x(900.0, 100.0, 200.0), 1.0);
    assert_eq!(fraction_from_x(f32::NAN, 100.0, 200.0), 0.0);
    assert_eq!(fraction_from_x(150.0, 100.0, 0.0), 0.0);
}

#[test]
fn seek_target_scales_and_keeps_off_the_very_end() {
    assert_eq!(seek_target_ms(0.0, Some(60_000)), Some(0));
    assert_eq!(seek_target_ms(0.5, Some(60_000)), Some(30_000));
    assert_eq!(
        seek_target_ms(1.0, Some(60_000)),
        Some(60_000 - SEEK_END_GUARD_MS)
    );
    assert_eq!(
        seek_target_ms(2.0, Some(60_000)),
        seek_target_ms(1.0, Some(60_000))
    );
    assert_eq!(seek_target_ms(f32::NAN, Some(60_000)), Some(0));
    assert_eq!(seek_target_ms(0.5, None), None);
    assert_eq!(seek_target_ms(0.5, Some(0)), None);
    // A track shorter than the guard seeks to its start.
    assert_eq!(seek_target_ms(0.9, Some(300)), Some(0));
}

// --------------------------------------------------------- transport ----

#[test]
fn play_key_follows_the_status() {
    let playing = state(PlaylistStatus::Playing);
    assert_eq!(transport_view(&playing, None, None).play, PlayAction::Pause);
    let paused = state(PlaylistStatus::Paused);
    assert_eq!(transport_view(&paused, None, None).play, PlayAction::Resume);
    assert!(transport_view(&playing, None, None).active);
    assert!(!transport_view(&state(PlaylistStatus::Stopped), None, None).active);
    assert!(!transport_view(&state(PlaylistStatus::Finished), None, None).active);
}

#[test]
fn stopped_transport_prefers_selected_then_last_then_first() {
    let mut stopped = state(PlaylistStatus::Stopped);
    assert_eq!(
        transport_view(&stopped, None, None).play,
        PlayAction::OpenManager
    );
    assert_eq!(
        transport_view(&stopped, None, Some("first")).play,
        PlayAction::Start("first".into())
    );
    stopped.playlist = Some("last".into());
    assert_eq!(
        transport_view(&stopped, None, Some("first")).play,
        PlayAction::Start("last".into())
    );
    assert_eq!(
        transport_view(&stopped, Some("picked"), Some("first")).play,
        PlayAction::Start("picked".into())
    );
}

#[test]
fn seek_needs_an_active_track_with_a_length() {
    let mut s = state(PlaylistStatus::Playing);
    assert!(!transport_view(&s, None, None).can_seek);
    s.duration_ms = Some(180_000);
    assert!(transport_view(&s, None, None).can_seek);
    s.status = PlaylistStatus::Stopped;
    assert!(!transport_view(&s, None, None).can_seek);
}

#[test]
fn manager_play_button_only_pauses_its_own_playlist() {
    let mut s = state(PlaylistStatus::Playing);
    s.playlist = Some("a".into());
    assert_eq!(playlist_play_action(&s, "a"), PlayAction::Pause);
    assert_eq!(playlist_play_action(&s, "b"), PlayAction::Start("b".into()));
    s.status = PlaylistStatus::Paused;
    assert_eq!(playlist_play_action(&s, "a"), PlayAction::Resume);
    s.status = PlaylistStatus::Finished;
    assert_eq!(playlist_play_action(&s, "a"), PlayAction::Start("a".into()));
}

#[test]
fn track_label_uses_tags_then_the_sound_name() {
    let mut s = state(PlaylistStatus::Playing);
    s.track = Some("id-1".into());
    s.track_index = Some(0);
    let names = |id: &str| (id == "id-1").then(|| "Tavern Theme".to_string());
    assert_eq!(track_display(&s, names).as_deref(), Some("Tavern Theme"));
    s.title = Some("Rest".into());
    assert_eq!(track_display(&s, names).as_deref(), Some("Rest"));
    s.artist = Some("Bard".into());
    assert_eq!(track_display(&s, names).as_deref(), Some("Bard - Rest"));
    s.track = Some("gone".into());
    s.title = None;
    s.artist = None;
    assert_eq!(track_display(&s, names).as_deref(), Some("Unknown track"));
    assert_eq!(track_display(&state(PlaylistStatus::Stopped), names), None);
}

#[test]
fn track_position_is_one_based() {
    let mut s = state(PlaylistStatus::Playing);
    assert_eq!(track_position_label(&s), None);
    s.track_index = Some(2);
    s.track_count = 12;
    assert_eq!(track_position_label(&s).as_deref(), Some("Track 3 / 12"));
}

#[test]
fn repeat_cycles_through_all_modes() {
    let mut mode = RepeatMode::Off;
    let mut seen = Vec::new();
    for _ in 0..3 {
        mode = next_repeat(mode);
        seen.push(mode);
    }
    assert_eq!(
        seen,
        vec![RepeatMode::All, RepeatMode::One, RepeatMode::Off]
    );
    assert_eq!(repeat_label(RepeatMode::One), "One");
}

#[test]
fn crossfade_labels_and_conversion() {
    assert_eq!(crossfade_label(0), "Off");
    assert_eq!(crossfade_label(2500), "2.5 s");
    assert_eq!(crossfade_ms_from_secs(2.5), 2500);
    assert_eq!(crossfade_ms_from_secs(-1.0), 0);
    assert_eq!(crossfade_ms_from_secs(f32::NAN), 0);
    assert_eq!(crossfade_ms_from_secs(1000.0), MAX_CROSSFADE_MS);
}

// ------------------------------------------------------- playlists ----

#[test]
fn new_playlists_get_unique_names() {
    let mut library = Library::new();
    let a = create_playlist(&mut library).unwrap();
    let b = create_playlist(&mut library).unwrap();
    let c = create_playlist(&mut library).unwrap();
    let names: Vec<_> = [a, b, c]
        .iter()
        .map(|id| library.playlist(*id).unwrap().name.clone())
        .collect();
    assert_eq!(names, ["New playlist", "New playlist 2", "New playlist 3"]);
    assert_eq!(unique_name("x", |_| false), "x");
}

#[test]
fn drop_slot_counts_rows_above_the_pointer() {
    let centers = [10.0, 30.0, 50.0];
    assert_eq!(drop_slot(&centers, 0.0), 0);
    assert_eq!(drop_slot(&centers, 20.0), 1);
    assert_eq!(drop_slot(&centers, 40.0), 2);
    assert_eq!(drop_slot(&centers, 999.0), 3);
    assert_eq!(drop_slot(&[], 5.0), 0);
}

#[test]
fn move_target_accounts_for_the_removed_row() {
    // 5 rows; dropping row 1 on gap 4 (between rows 3 and 4) -> index 3.
    assert_eq!(move_target(5, 1, 4), 3);
    // Dropping on its own gaps changes nothing.
    assert_eq!(move_target(5, 2, 2), 2);
    assert_eq!(move_target(5, 2, 3), 2);
    // Up and to the very end / start.
    assert_eq!(move_target(5, 4, 0), 0);
    assert_eq!(move_target(5, 0, 5), 4);
    assert_eq!(move_target(5, 0, 99), 4);
    assert_eq!(move_target(1, 0, 1), 0);
}

#[test]
fn tracks_move_by_buttons_and_by_drag() {
    let (mut library, ids) = library_with_sounds(4);
    let pl = library.add_playlist("Mix").unwrap();
    assert!(library.set_playlist_items(pl, ids.clone()));
    let order = |l: &Library| l.playlist(pl).unwrap().items.clone();

    // "Move down" on row 0 is slot 2.
    assert!(move_track(&mut library, pl, 0, 2));
    assert_eq!(order(&library), vec![ids[1], ids[0], ids[2], ids[3]]);
    // "Move up" on row 3 is slot 2.
    assert!(move_track(&mut library, pl, 3, 2));
    assert_eq!(order(&library), vec![ids[1], ids[0], ids[3], ids[2]]);
    // Drag row 1 to the end.
    assert!(move_track(&mut library, pl, 1, 4));
    assert_eq!(order(&library), vec![ids[1], ids[3], ids[2], ids[0]]);
    // No-ops.
    assert!(!move_track(&mut library, pl, 1, 1));
    assert!(!move_track(&mut library, pl, 9, 0));
    assert!(!move_track(&mut library, uuid::Uuid::new_v4(), 0, 1));
    assert_eq!(order(&library), vec![ids[1], ids[3], ids[2], ids[0]]);
}

#[test]
fn sound_filter_matches_names_and_tags() {
    let (mut library, _) = library_with_sounds(3);
    let path = library.sounds()[1].path.clone();
    library.add_tag(&path, "Battle");
    let all = filter_sounds(library.sounds(), "  ", 10);
    assert_eq!(all.len(), 3);
    let by_name = filter_sounds(library.sounds(), "sound 2", 10);
    assert_eq!(by_name.len(), 1);
    let by_tag = filter_sounds(library.sounds(), "BATTLE", 10);
    assert_eq!(by_tag.len(), 1);
    assert_eq!(by_tag[0].path, path);
    assert_eq!(filter_sounds(library.sounds(), "", 2).len(), 2);
    assert!(filter_sounds(library.sounds(), "nothing", 10).is_empty());
}

#[test]
fn sound_label_falls_back_to_the_file_stem() {
    let (mut library, ids) = library_with_sounds(1);
    assert_eq!(sound_name(&library, ids[0]), "Sound 0");
    let path = library.sounds()[0].path.clone();
    library.sound_mut(&path).unwrap().name = String::new();
    assert_eq!(sound_name(&library, ids[0]), "bardwave-test-0");
    assert_eq!(sound_name(&library, uuid::Uuid::new_v4()), "Missing sound");
}

// ---------------------------------------------------------- emitters ----

#[test]
fn countdown_text_covers_every_state() {
    assert_eq!(format_countdown(0), "now");
    assert_eq!(format_countdown(1), "1 s");
    assert_eq!(format_countdown(1001), "2 s");
    assert_eq!(format_countdown(59_000), "59 s");
    assert_eq!(format_countdown(61_500), "1 min 02 s");

    assert_eq!(countdown_label(None), "Scene not active");
    let mut s = EmitterSnapshot {
        index: 0,
        enabled: false,
        next_in_ms: None,
        active: 0,
    };
    assert_eq!(countdown_label(Some(&s)), "No playable sounds");
    s.enabled = true;
    s.next_in_ms = Some(12_300);
    assert_eq!(countdown_label(Some(&s)), "Next in 13 s");
    s.active = 2;
    assert_eq!(countdown_label(Some(&s)), "Next in 13 s · 2 playing");
    s.next_in_ms = None;
    assert_eq!(countdown_label(Some(&s)), "Waiting · 2 playing");
}

#[test]
fn seconds_and_interval_labels() {
    assert_eq!(format_seconds(20_000), "20 s");
    assert_eq!(format_seconds(1_500), "1.5 s");
    assert_eq!(format_seconds(125_000), "2 min 05 s");
    assert_eq!(interval_label(20_000, 60_000), "20 s - 60 s");
}

#[test]
fn interval_edits_keep_min_below_max_in_range() {
    assert_eq!(
        set_interval(20_000, 60_000, IntervalEdit::Min(30_000)),
        (30_000, 60_000)
    );
    // Raising the minimum past the maximum drags the maximum along.
    assert_eq!(
        set_interval(20_000, 60_000, IntervalEdit::Min(90_000)),
        (90_000, 90_000)
    );
    // Lowering the maximum below the minimum drags the minimum.
    assert_eq!(
        set_interval(20_000, 60_000, IntervalEdit::Max(5_000)),
        (5_000, 5_000)
    );
    // Out of range values are clamped.
    assert_eq!(
        set_interval(20_000, 60_000, IntervalEdit::Min(0)).0,
        MIN_EMITTER_INTERVAL_MS
    );
    assert_eq!(
        set_interval(20_000, 60_000, IntervalEdit::Max(u32::MAX)).1,
        MAX_EMITTER_INTERVAL_MS
    );
    // Whatever is typed, the pair stays ordered and in range.
    for min in [100, 5_000, 2_000_000, 3_600_000] {
        for max in [100, 5_000, 2_000_000, 3_600_000] {
            for value in [0, 99, 100, 5_000, 3_600_000, u32::MAX] {
                for edit in [IntervalEdit::Min(value), IntervalEdit::Max(value)] {
                    let (lo, hi) = set_interval(min.min(max), max, edit);
                    assert!(lo <= hi, "{lo} > {hi}");
                    assert!(lo >= MIN_EMITTER_INTERVAL_MS && hi <= MAX_EMITTER_INTERVAL_MS);
                }
            }
        }
    }
}

#[test]
fn draft_edits_stay_out_of_the_library_until_committed() {
    let (mut library, ids) = library_with_sounds(2);
    let scene = library.add_scene("Forest").unwrap();
    let theme = library.add_playlist("Theme").unwrap();

    let mut draft = EmitterDraft::from_library(&library, scene).unwrap();
    assert!(!draft.is_dirty(&library));
    draft.emitters.push(Emitter::new(Vec::new()));
    draft.playlist = Some(theme);
    assert!(draft.is_dirty(&library));
    assert_eq!(
        draft.issue().as_deref(),
        Some("Emitter 1: add at least one sound")
    );
    // An emitter without sounds cannot be saved.
    assert!(!draft.commit(&mut library));
    assert!(library.scene(scene).unwrap().emitters.is_empty());
    assert_eq!(library.scene(scene).unwrap().playlist, None);

    draft.emitters[0].sounds.push(ids[0]);
    draft.emitters[0].min_interval_ms = 90_000;
    draft.emitters[0].max_interval_ms = 30_000;
    assert_eq!(draft.issue(), None);
    assert!(draft.commit(&mut library));
    let saved = library.scene(scene).unwrap();
    assert_eq!(saved.playlist, Some(theme));
    assert_eq!(saved.emitters.len(), 1);
    // The library sanitized the reversed interval.
    assert!(saved.emitters[0].min_interval_ms <= saved.emitters[0].max_interval_ms);
}

#[test]
fn draft_commit_is_refused_for_vanished_things() {
    let (mut library, ids) = library_with_sounds(1);
    let scene = library.add_scene("Cave").unwrap();
    let theme = library.add_playlist("Drips").unwrap();
    let mut draft = EmitterDraft::from_library(&library, scene).unwrap();
    draft.emitters.push(Emitter::new(vec![ids[0]]));
    draft.playlist = Some(theme);
    assert!(library.remove_playlist(theme));
    assert!(!draft.commit(&mut library), "the playlist is gone");
    assert!(library.scene(scene).unwrap().emitters.is_empty());

    draft.playlist = None;
    assert!(library.remove_scene(scene));
    assert!(!draft.commit(&mut library), "the scene is gone");
    assert!(EmitterDraft::from_library(&library, scene).is_none());
}

#[test]
fn list_heights_stay_in_bounds() {
    for h in [300.0, 620.0, 800.0, 1400.0, 4000.0] {
        let (tracks, picker) = list_heights(h);
        assert!((90.0..=205.0).contains(&tracks), "{tracks}");
        assert!((55.0..=126.0).contains(&picker), "{picker}");
    }
}

// ------------------------------------------------------------- state ----

#[test]
fn repaint_follows_what_is_on_screen() {
    let mut ui = MusicUi::default();
    assert!(ui.repaint_hint(&Dialog::None) >= Duration::from_secs(60));
    ui.live.status = PlaylistStatus::Playing;
    assert_eq!(ui.repaint_hint(&Dialog::None), Duration::from_millis(250));
    ui.live.status = PlaylistStatus::Stopped;
    ui.emitters.push(EmitterSnapshot {
        index: 0,
        enabled: true,
        next_in_ms: Some(1),
        active: 0,
    });
    let dialog = Dialog::EditEmitters(uuid::Uuid::new_v4());
    assert_eq!(ui.repaint_hint(&dialog), Duration::from_millis(200));
    assert!(ui.repaint_hint(&Dialog::None) >= Duration::from_secs(60));
}

#[test]
fn observe_copies_the_directors_state() {
    let director = Director::new();
    let mut ui = MusicUi::default();
    ui.observe(&director, Instant::now());
    assert_eq!(ui.live().status, PlaylistStatus::Stopped);
    assert!(ui.emitters.is_empty());
    assert_eq!(ui.active_scene, None);
}

// ----------------------------------------------------------- drawing ----

/// Run `draw` for two passes in a headless egui context.
fn run_frames(mut draw: impl FnMut(&mut Ui)) {
    let ctx = egui::Context::default();
    for _ in 0..2 {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 800.0))),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| draw(ui));
        });
    }
}

fn env_for<'a>(library: &'a Library, ui: &MusicUi) -> MusicEnv<'a> {
    let durations = library.sounds().iter().map(|s| (s.id, 95.0)).collect();
    MusicEnv::new(library, ui, durations)
}

#[test]
fn idle_transport_bar_draws_and_asks_for_nothing() {
    let library = Library::new();
    let mut st = MusicUi::default();
    run_frames(|ui| {
        let env = env_for(&library, &st);
        assert!(draw_bar(ui, &env, &mut st).is_empty());
    });
}

#[test]
fn playing_transport_bar_draws() {
    let (mut library, ids) = library_with_sounds(2);
    let pl = library.add_playlist("Road").unwrap();
    library.set_playlist_items(pl, ids.clone());
    let mut st = MusicUi {
        live: PlaylistState {
            playlist: Some(pl.to_string()),
            name: Some("Road".into()),
            status: PlaylistStatus::Playing,
            track_count: 2,
            track_index: Some(1),
            track: Some(ids[1].to_string()),
            title: Some("A very long title that must be elided somewhere".into()),
            artist: Some("Someone".into()),
            position_ms: 42_000,
            duration_ms: Some(180_000),
            shuffle: true,
            repeat: RepeatMode::One,
            ..Default::default()
        },
        message: Some("could not start".into()),
        ..Default::default()
    };
    run_frames(|ui| {
        let env = env_for(&library, &st);
        let _ = draw_bar(ui, &env, &mut st);
    });
}

#[test]
fn manager_draws_both_tabs_with_content_and_when_empty() {
    let empty = Library::new();
    let mut st = MusicUi::default();
    run_frames(|ui| {
        let env = env_for(&empty, &st);
        assert!(draw_manager(ui, &env, &mut st, 800.0).is_empty());
    });

    let (mut library, ids) = library_with_sounds(5);
    let pl = library.add_playlist("Tavern").unwrap();
    library.set_playlist_items(pl, ids.clone());
    library.add_playlist("Empty");
    let scene = library.add_scene("Inn").unwrap();
    library.set_scene_playlist(scene, Some(pl));
    let mut st = MusicUi {
        rename: Some((pl, "Tavern".into())),
        ..Default::default()
    };
    run_frames(|ui| {
        let env = env_for(&library, &st);
        let _ = draw_manager(ui, &env, &mut st, 620.0);
    });
    assert_eq!(st.selected, Some(pl), "the first playlist is selected");
    st.tab = MusicTab::Scenes;
    st.rename = None;
    run_frames(|ui| {
        let env = env_for(&library, &st);
        let _ = draw_manager(ui, &env, &mut st, 620.0);
    });
}

#[test]
fn manager_survives_a_selection_that_was_deleted() {
    let (mut library, _) = library_with_sounds(1);
    let gone = library.add_playlist("Gone").unwrap();
    library.add_playlist("Stays").unwrap();
    library.remove_playlist(gone);
    let mut st = MusicUi {
        selected: Some(gone),
        confirm_delete: Some(gone),
        ..Default::default()
    };
    run_frames(|ui| {
        let env = env_for(&library, &st);
        let _ = draw_manager(ui, &env, &mut st, 800.0);
    });
    assert_ne!(st.selected, Some(gone));
}

#[test]
fn emitters_dialog_draws_a_running_scene() {
    let (mut library, ids) = library_with_sounds(3);
    let scene = library.add_scene("Forest").unwrap();
    let theme = library.add_playlist("Birds").unwrap();
    let mut crows = Emitter::new(vec![ids[0], ids[1]]);
    crows.volume_jitter = 0.2;
    let drips = Emitter::new(Vec::new());
    assert!(library.set_emitters(scene, vec![crows]));
    library.set_scene_playlist(scene, Some(theme));

    let mut st = MusicUi {
        draft: EmitterDraft::from_library(&library, scene),
        active_scene: Some(scene.to_string()),
        emitters: vec![EmitterSnapshot {
            index: 0,
            enabled: true,
            next_in_ms: Some(8_000),
            active: 1,
        }],
        ..Default::default()
    };
    st.draft.as_mut().unwrap().emitters.push(drips);
    run_frames(|ui| {
        let env = env_for(&library, &st);
        let actions = draw_emitters(ui, &env, &mut st, scene, 700.0);
        assert!(actions.is_empty());
    });
    // Drawing never writes to the library.
    assert_eq!(library.scene(scene).unwrap().emitters.len(), 1);
}

#[test]
fn emitters_dialog_for_an_unknown_scene_draws_nothing() {
    let library = Library::new();
    let mut st = MusicUi::default();
    run_frames(|ui| {
        let env = env_for(&library, &st);
        assert!(draw_emitters(ui, &env, &mut st, uuid::Uuid::new_v4(), 700.0).is_empty());
    });
}

// ------------------------------------------------- transport bar layout ----

/// Inner width of the card for a window of `w` points.
fn inner_of(window: f32) -> f32 {
    window - 2.0 * Metrics::APP_PAD - 2.0 * PLAYER_PAD
}

#[test]
fn player_items_fit_the_card_at_any_width() {
    for window in 640..=1600 {
        let inner = inner_of(window as f32);
        let layout = player_layout(inner);
        assert!(
            layout.width() <= inner + 0.001,
            "{window}: {layout:?} needs {} of {inner}",
            layout.width()
        );
        assert!(layout.seek_w >= SEEK_MIN_W, "{window}: {layout:?}");
        assert!(layout.now_playing_w >= NOW_PLAYING_MIN_W, "{window}");
        assert!(layout.now_playing_w <= NOW_PLAYING_W, "{window}");
    }
}

#[test]
fn player_gives_up_in_the_agreed_order() {
    let roomy = player_layout(inner_of(1600.0));
    assert!(roomy.mode_labels && roomy.clocks);
    assert_eq!(roomy.now_playing_w, NOW_PLAYING_W);
    assert!(roomy.seek_w > SEEK_MIN_W);

    let mut previous = roomy;
    for window in (640..=1600).rev() {
        let l = player_layout(inner_of(window as f32));
        // The seek bar shrinks first: the block only gets narrower once the
        // bar sits at its minimum.
        if l.now_playing_w < NOW_PLAYING_W && l.mode_labels == previous.mode_labels {
            assert_eq!(l.seek_w, SEEK_MIN_W, "{window}: {l:?}");
        }
        // Labels become icons before the clocks go.
        if !l.clocks {
            assert!(!l.mode_labels, "{window}: {l:?}");
        }
        // Nothing that was given up comes back as the window narrows.
        assert!(l.clocks <= previous.clocks && l.mode_labels <= previous.mode_labels);
        previous = l;
    }
}

#[test]
fn player_at_900_keeps_its_labels_and_a_usable_seek_bar() {
    let l = player_layout(inner_of(900.0));
    assert!(l.mode_labels && l.clocks, "{l:?}");
    assert!(l.seek_w >= SEEK_MIN_W);
    // 640 is tight enough for the icon form.
    assert!(!player_layout(inner_of(640.0)).mode_labels);
}

#[test]
fn card_sits_between_equal_gaps() {
    let panel = Rect::from_min_size(pos2(0.0, 500.0), vec2(1000.0, Metrics::PLAYER_BAR_H));
    let card = player_card_rect(panel);
    assert_eq!(card.top() - panel.top(), Metrics::PLAYER_GAP_TOP);
    assert_eq!(panel.bottom() - card.bottom(), Metrics::PLAYER_GAP_BOTTOM);
    assert_eq!(card.height(), Metrics::PLAYER_CARD_H);
    assert_eq!(card.left(), Metrics::APP_PAD);
}

fn draw_player_at(
    window: f32,
    st: &mut MusicUi,
    library: &Library,
) -> Vec<crate::gui::layout_probe::TextBox> {
    use crate::gui::layout_probe::{run_frames as probe_frames, text_boxes};
    let ctx = egui::Context::default();
    let out = probe_frames(&ctx, vec2(window, 300.0), 2, |ctx| {
        egui::TopBottomPanel::bottom("music_bar")
            .exact_height(Metrics::PLAYER_BAR_H)
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                let env = env_for(library, st);
                let _ = draw_bar(ui, &env, st);
            });
    });
    text_boxes(&out)
}

#[test]
fn painted_player_stays_inside_its_card_at_any_width() {
    use crate::gui::layout_probe::assert_clean;
    let (mut library, ids) = library_with_sounds(2);
    let pl = library
        .add_playlist("A playlist with a rather long name")
        .unwrap();
    library.set_playlist_items(pl, ids.clone());
    let playing = |repeat: RepeatMode| MusicUi {
        live: PlaylistState {
            playlist: Some(pl.to_string()),
            name: Some("A playlist with a rather long name".into()),
            status: PlaylistStatus::Playing,
            track_count: 2,
            track_index: Some(1),
            track: Some(ids[1].to_string()),
            title: Some("A very long title that must be elided somewhere".into()),
            artist: Some("Someone".into()),
            position_ms: 42_000,
            duration_ms: Some(180_000),
            shuffle: true,
            repeat,
            ..Default::default()
        },
        message: Some("could not start the track because of something long".into()),
        ..Default::default()
    };
    for window in (640..=1600).step_by(8).chain([900, 960]) {
        let window = window as f32;
        let card = player_card_rect(Rect::from_min_max(
            pos2(0.0, 300.0 - Metrics::PLAYER_BAR_H),
            pos2(window, 300.0),
        ));
        for (name, mut st) in [
            ("idle", MusicUi::default()),
            ("playing", playing(RepeatMode::One)),
            ("repeat all", playing(RepeatMode::All)),
        ] {
            let boxes = draw_player_at(window, &mut st, &library);
            assert!(!boxes.is_empty(), "{window} {name}: nothing painted");
            assert_clean(&boxes, card, &format!("player ({name}) at {window}"));
        }
    }
}

#[test]
fn narrow_player_shows_icons_with_the_same_tooltips_and_wide_one_shows_words() {
    let library = Library::new();
    let wide = draw_player_at(1280.0, &mut MusicUi::default(), &library);
    assert!(wide.iter().any(|b| b.text == "SHUFFLE"), "{wide:?}");
    assert!(
        wide.iter().any(|b| b.text.starts_with("REPEAT")),
        "{wide:?}"
    );
    let narrow = draw_player_at(640.0, &mut MusicUi::default(), &library);
    assert!(!narrow.iter().any(|b| b.text == "SHUFFLE"), "{narrow:?}");
    assert!(narrow.iter().any(|b| b.text == "NO MUSIC PLAYING"));
    assert!(narrow.iter().any(|b| b.text == "MUSIC…"));
}

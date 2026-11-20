//! The music GUI's model end to end, without a window: library edits made
//! with the panel's helpers reach the Director, and what the Director reports
//! turns into the texts and buttons the panel shows.
//!
//! The playback is the mock with an artificial clock, so nothing sleeps and no
//! sound card or config file is touched.

use std::path::Path;

use soundshared::audio::director::mock::MockCall;
use soundshared::audio::playlist::mock::MusicMock;
use soundshared::audio::{
    CatalogSound, Director, PlaylistCommand, PlaylistStatus, SoundId, TrackMeta,
};
use soundshared::gui::groups::{Emitter, Library};
use soundshared::gui::music::{
    countdown_label, create_playlist, move_track, progress_fraction, seek_target_ms, track_display,
    transport_view, EmitterDraft, PlayAction,
};

const TRACK_MS: u64 = 60_000;

struct Rig {
    pb: MusicMock,
    director: Director,
    library: Library,
    /// Library ids of the sounds, in creation order.
    ids: Vec<uuid::Uuid>,
    engine_ids: Vec<SoundId>,
}

impl Rig {
    fn new(tracks: usize) -> Self {
        let pb = MusicMock::new();
        let mut library = Library::new();
        let mut engine_ids = Vec::new();
        for i in 0..tracks {
            let meta = TrackMeta {
                title: (i == 1).then(|| "Second Wind".to_string()),
                artist: (i == 1).then(|| "The Bard".to_string()),
                album: None,
            };
            engine_ids.push(pb.add_track_with_meta(TRACK_MS, meta));
            let path = format!("/tmp/bardwave-gui-music-{i}.ogg");
            assert!(library.insert_sound(Path::new(&path), &format!("Track {i}"), 1.0, None));
        }
        let ids = library.sounds().iter().map(|s| s.id).collect();
        let mut rig = Self {
            pb,
            director: Director::new(),
            library,
            ids,
            engine_ids,
        };
        rig.sync();
        rig
    }

    /// What `sync_director` does in the app, with the mock's sound ids.
    fn sync(&mut self) {
        let config = self.library.to_config();
        let catalog = self
            .library
            .sounds()
            .iter()
            .zip(&self.engine_ids)
            .map(|(sound, engine)| CatalogSound::new(sound.id.to_string(), *engine))
            .collect();
        self.director.sync_library(config.scenes, catalog);
        self.director.set_playlists(config.playlists);
    }

    fn run(&mut self, ms: u64) {
        let mut left = ms;
        while left > 0 {
            let step = left.min(250);
            self.pb.advance(step);
            self.director.tick_at(&self.pb, self.pb.now());
            left -= step;
        }
    }

    fn command(&mut self, command: PlaylistCommand) {
        self.sync();
        self.director.playlist_command(&self.pb, command).unwrap();
    }
}

#[test]
fn a_playlist_built_in_the_panel_plays_and_shows_its_state() {
    let mut rig = Rig::new(3);
    let id = create_playlist(&mut rig.library).unwrap();
    for sound in rig.ids.clone() {
        assert!(rig.library.add_playlist_item(id, sound, None));
    }
    assert!(rig.library.set_playlist_crossfade(id, 0));
    assert!(rig
        .library
        .set_playlist_mode(id, false, soundshared::config::RepeatMode::All));

    // Stopped: the transport offers to start the only playlist.
    let state = rig.director.playlist_state();
    let first = rig.library.playlists()[0].id.to_string();
    assert_eq!(
        transport_view(&state, None, Some(&first)).play,
        PlayAction::Start(first.clone())
    );

    rig.command(PlaylistCommand::Start(id.to_string()));
    rig.run(1_000);
    let state = rig.director.playlist_state();
    assert_eq!(state.status, PlaylistStatus::Playing);
    assert_eq!(state.track_count, 3);
    assert_eq!(state.track_index, Some(0));
    let view = transport_view(&state, None, None);
    assert_eq!(view.play, PlayAction::Pause);
    assert!(view.active && view.can_seek);

    // The track without tags is labelled with the sound's name.
    let ids = rig.ids.clone();
    let library = &rig.library;
    let name_of = |track: &str| {
        let id = uuid::Uuid::parse_str(track).ok()?;
        library.sound_by_id(id).map(|s| s.name.clone())
    };
    assert_eq!(track_display(&state, name_of).as_deref(), Some("Track 0"));

    // Next: the second track carries tags, which win over the name.
    rig.command(PlaylistCommand::Next);
    let state = rig.director.playlist_state();
    assert_eq!(state.track_index, Some(1));
    let library = &rig.library;
    let name_of = |track: &str| {
        let id = uuid::Uuid::parse_str(track).ok()?;
        library.sound_by_id(id).map(|s| s.name.clone())
    };
    assert_eq!(
        track_display(&state, name_of).as_deref(),
        Some("The Bard - Second Wind")
    );
    assert_eq!(state.track.as_deref(), Some(ids[1].to_string().as_str()));
}

#[test]
fn seeking_from_the_bar_moves_the_position() {
    let mut rig = Rig::new(1);
    let id = create_playlist(&mut rig.library).unwrap();
    rig.library.add_playlist_item(id, rig.ids[0], None);
    rig.command(PlaylistCommand::Start(id.to_string()));
    rig.run(500);
    let state = rig.director.playlist_state();
    assert_eq!(state.duration_ms, Some(TRACK_MS));
    assert!(progress_fraction(state.position_ms, state.duration_ms) < 0.05);

    // A click at 75 % of the bar.
    let target = seek_target_ms(0.75, state.duration_ms).unwrap();
    assert_eq!(target, 45_000);
    rig.command(PlaylistCommand::Seek(target));
    rig.run(250);
    let state = rig.director.playlist_state();
    let fraction = progress_fraction(state.position_ms, state.duration_ms);
    assert!((0.74..0.8).contains(&fraction), "fraction {fraction}");

    // Pause / resume follow the status the play key reports.
    rig.command(PlaylistCommand::Pause);
    let state = rig.director.playlist_state();
    assert_eq!(state.status, PlaylistStatus::Paused);
    assert_eq!(transport_view(&state, None, None).play, PlayAction::Resume);
    rig.command(PlaylistCommand::Resume);
    assert_eq!(
        rig.director.playlist_state().status,
        PlaylistStatus::Playing
    );
}

#[test]
fn reordering_a_playing_playlist_changes_what_plays_next() {
    let mut rig = Rig::new(3);
    let id = create_playlist(&mut rig.library).unwrap();
    for sound in rig.ids.clone() {
        rig.library.add_playlist_item(id, sound, None);
    }
    rig.library.set_playlist_crossfade(id, 0);
    rig.command(PlaylistCommand::Start(id.to_string()));
    rig.run(500);

    // Drag the last track to the top; the track that plays stays current.
    assert!(move_track(&mut rig.library, id, 2, 0));
    rig.sync();
    rig.run(500);
    let state = rig.director.playlist_state();
    assert_eq!(state.status, PlaylistStatus::Playing);
    assert_eq!(state.track_index, Some(1), "track 0 moved down one place");
    assert_eq!(
        state.track.as_deref(),
        Some(rig.ids[0].to_string().as_str())
    );

    // Next goes to the track that followed it in the new order.
    rig.command(PlaylistCommand::Next);
    assert_eq!(
        rig.director.playlist_state().track.as_deref(),
        Some(rig.ids[1].to_string().as_str())
    );
}

#[test]
fn shuffle_and_repeat_set_from_the_panel_reach_the_player() {
    let mut rig = Rig::new(3);
    let id = create_playlist(&mut rig.library).unwrap();
    for sound in rig.ids.clone() {
        rig.library.add_playlist_item(id, sound, None);
    }
    rig.command(PlaylistCommand::Start(id.to_string()));
    rig.run(250);
    assert!(!rig.director.playlist_state().shuffle);

    rig.command(PlaylistCommand::SetShuffle(true));
    rig.command(PlaylistCommand::SetRepeat(
        soundshared::config::RepeatMode::One,
    ));
    let state = rig.director.playlist_state();
    assert!(state.shuffle);
    assert_eq!(state.repeat, soundshared::config::RepeatMode::One);
}

#[test]
fn a_saved_emitter_draft_starts_with_the_scene_and_reports_a_countdown() {
    let mut rig = Rig::new(2);
    let scene = rig.library.add_scene("Forest").unwrap();
    let theme = create_playlist(&mut rig.library).unwrap();
    rig.library.add_playlist_item(theme, rig.ids[1], None);

    // Nothing runs before the dialog saves.
    let mut draft = EmitterDraft::from_library(&rig.library, scene).unwrap();
    let mut crows = Emitter::new(vec![rig.ids[0]]);
    crows.min_interval_ms = 2_000;
    crows.max_interval_ms = 4_000;
    draft.emitters.push(crows);
    draft.playlist = Some(theme);
    assert!(draft.commit(&mut rig.library));
    rig.sync();

    let now = rig.pb.now();
    rig.director
        .activate_scene_at(&rig.pb, &scene.to_string(), now)
        .unwrap();
    // The scene's music layer started with it.
    assert_eq!(
        rig.director.playlist_state().playlist.as_deref(),
        Some(theme.to_string().as_str())
    );

    let snapshot = rig.director.emitter_snapshot(rig.pb.now());
    assert_eq!(snapshot.len(), 1);
    let label = countdown_label(snapshot.first());
    assert!(label.starts_with("Next in "), "{label}");

    // After a while the crow has played.
    rig.run(12_000);
    let plays = rig
        .pb
        .calls()
        .into_iter()
        .filter(|c| matches!(c, MockCall::OneShot { sound, .. } if *sound == rig.engine_ids[0]))
        .count();
    assert!(plays >= 1, "the emitter played {plays} times");

    // Without the scene there is no countdown to show.
    rig.director.stop_scene(&rig.pb).unwrap();
    assert!(rig.director.emitter_snapshot(rig.pb.now()).is_empty());
    assert_eq!(countdown_label(None), "Scene not active");
}

//! Music playlists through the Director, on a mock playback with positions
//! and an artificial clock: no sound card, no files, no sleeping.
//!
//! Every test builds a catalog of fake tracks (ids `t0`, `t1`, ...), moves the
//! clock with `Rig::run` (advance the mock, then tick the Director) and checks
//! what the player started and stopped.

use std::collections::HashSet;
use std::time::Instant;

use soundshared::audio::director::mock::MockCall;
use soundshared::audio::playlist::mock::MusicMock;
use soundshared::audio::{
    CatalogSound, Director, DirectorCommand, Playback, PlaylistCommand, PlaylistStatus, SoundId,
    TrackMeta,
};
use soundshared::config::{Bus, PlaylistConfig, RepeatMode, SceneConfig};

const TRACK_MS: u64 = 10_000;
const STEP_MS: u64 = 250;

struct Rig {
    pb: MusicMock,
    director: Director,
    sounds: Vec<SoundId>,
    scenes: Vec<SceneConfig>,
    catalog: Vec<CatalogSound>,
}

fn playlist(
    id: &str,
    items: &[&str],
    shuffle: bool,
    repeat: RepeatMode,
    xf: u32,
) -> PlaylistConfig {
    PlaylistConfig {
        id: id.into(),
        name: id.into(),
        items: items.iter().map(|s| s.to_string()).collect(),
        shuffle,
        repeat,
        crossfade_ms: xf,
        bus: Bus::Music,
    }
}

fn scene(id: &str, playlist: Option<&str>, stop_others: bool) -> SceneConfig {
    SceneConfig {
        id: id.into(),
        name: id.into(),
        fade_in_ms: 1000,
        fade_out_ms: 2000,
        stop_others,
        playlist: playlist.map(str::to_string),
        ..Default::default()
    }
}

impl Rig {
    /// `n` tracks `t0..` of `TRACK_MS` each.
    fn new(n: usize) -> Self {
        let pb = MusicMock::new();
        let mut sounds = Vec::new();
        let mut catalog = Vec::new();
        for i in 0..n {
            let sound = pb.add_track(TRACK_MS);
            sounds.push(sound);
            catalog.push(CatalogSound::new(format!("t{i}"), sound));
        }
        let mut director = Director::new();
        director.sync_library(Vec::new(), catalog.clone());
        Self {
            pb,
            director,
            sounds,
            scenes: Vec::new(),
            catalog,
        }
    }

    fn sync(&mut self, playlists: Vec<PlaylistConfig>) {
        self.director
            .sync_library(self.scenes.clone(), self.catalog.clone());
        self.director.set_playlists(playlists);
    }

    fn with_playlists(mut self, playlists: Vec<PlaylistConfig>) -> Self {
        self.sync(playlists);
        self
    }

    fn with_scenes(mut self, scenes: Vec<SceneConfig>, playlists: Vec<PlaylistConfig>) -> Self {
        self.scenes = scenes;
        self.sync(playlists);
        self
    }

    fn tick(&mut self) {
        self.director.tick_at(&self.pb, self.pb.now());
    }

    /// Let `ms` pass in `STEP_MS` steps, ticking after each.
    fn run(&mut self, ms: u64) {
        let mut left = ms;
        while left > 0 {
            let step = left.min(STEP_MS);
            self.pb.advance(step);
            self.tick();
            left -= step;
        }
    }

    fn index_of(&self, sound: SoundId) -> usize {
        self.sounds.iter().position(|s| *s == sound).unwrap()
    }

    /// Indices of the tracks started so far.
    fn started(&self) -> Vec<usize> {
        self.pb
            .start_log()
            .into_iter()
            .map(|s| self.index_of(s))
            .collect()
    }

    fn state(&self) -> soundshared::audio::PlaylistState {
        self.director.playlist_state()
    }
}

// ------------------------------------------------------------ shuffle ----

#[test]
fn shuffle_plays_every_track_before_repeating_and_never_repeats_across_cycles() {
    for seed in 0..40 {
        let mut rig = Rig::new(5).with_playlists(vec![playlist(
            "pl",
            &["t0", "t1", "t2", "t3", "t4"],
            true,
            RepeatMode::All,
            0,
        )]);
        rig.director.seed_music(seed);
        rig.director.start_playlist(&rig.pb, "pl").unwrap();
        let mut guard = 0;
        while rig.started().len() < 20 && guard < 4000 {
            rig.run(STEP_MS);
            guard += 1;
        }
        let order = rig.started();
        assert!(
            order.len() >= 20,
            "seed {seed}: only {} starts",
            order.len()
        );
        for cycle in order.chunks(5).take(4) {
            let unique: HashSet<_> = cycle.iter().collect();
            assert_eq!(
                unique.len(),
                5,
                "seed {seed}: a track repeated inside {cycle:?}"
            );
        }
        for pair in order.chunks(5).take(4).collect::<Vec<_>>().windows(2) {
            assert_ne!(
                pair[0][4], pair[1][0],
                "seed {seed}: the next cycle started with the last track ({order:?})"
            );
        }
    }
}

#[test]
fn shuffled_start_differs_between_seeds() {
    let mut firsts = HashSet::new();
    for seed in 0..30 {
        let mut rig = Rig::new(6).with_playlists(vec![playlist(
            "pl",
            &["t0", "t1", "t2", "t3", "t4", "t5"],
            true,
            RepeatMode::All,
            0,
        )]);
        rig.director.seed_music(seed);
        rig.director.start_playlist(&rig.pb, "pl").unwrap();
        firsts.insert(rig.started()[0]);
    }
    assert!(firsts.len() > 2, "shuffle always starts with {firsts:?}");
}

#[test]
fn one_track_listed_twice_never_plays_back_to_back_when_shuffled() {
    for seed in 0..30 {
        let mut rig = Rig::new(2).with_playlists(vec![playlist(
            "pl",
            &["t0", "t0", "t1"],
            true,
            RepeatMode::All,
            0,
        )]);
        rig.director.seed_music(seed);
        rig.director.start_playlist(&rig.pb, "pl").unwrap();
        while rig.started().len() < 12 {
            rig.run(STEP_MS);
        }
        let order = rig.started();
        let cycles: Vec<_> = order.chunks(3).take(3).collect();
        for pair in cycles.windows(2) {
            assert_ne!(pair[0][2], pair[1][0], "seed {seed}: {order:?}");
        }
    }
}

#[test]
fn turning_shuffle_on_and_off_keeps_the_current_track() {
    let mut rig = Rig::new(4).with_playlists(vec![playlist(
        "pl",
        &["t0", "t1", "t2", "t3"],
        false,
        RepeatMode::All,
        0,
    )]);
    rig.director.start_playlist(&rig.pb, "pl").unwrap();
    rig.run(TRACK_MS); // now t1
    assert_eq!(rig.state().track.as_deref(), Some("t1"));
    rig.director
        .playlist_command(&rig.pb, PlaylistCommand::SetShuffle(true))
        .unwrap();
    assert!(rig.state().shuffle);
    assert_eq!(rig.state().track.as_deref(), Some("t1"));
    assert_eq!(rig.pb.starts_of(rig.sounds[1]), 1, "not restarted");
    // The rest of the cycle never contains t1 again.
    let before = rig.started().len();
    rig.run(TRACK_MS * 3);
    let cycle: Vec<_> = rig.started()[before..].to_vec();
    assert_eq!(cycle.len(), 3);
    assert!(!cycle.contains(&1), "{cycle:?}");
    // Back to order mode: continues in order from the current track.
    rig.director
        .playlist_command(&rig.pb, PlaylistCommand::SetShuffle(false))
        .unwrap();
    let current = rig.index_of(
        rig.director
            .catalog()
            .iter()
            .find(|c| Some(c.id.as_str()) == rig.state().track.as_deref())
            .unwrap()
            .sound,
    );
    rig.run(TRACK_MS);
    let next = rig.started().last().copied().unwrap();
    assert_eq!(next, (current + 1) % 4);
}

// ------------------------------------------------------------- repeat ----

#[test]
fn repeat_off_plays_the_list_once_and_finishes() {
    let mut rig = Rig::new(3).with_playlists(vec![playlist(
        "pl",
        &["t0", "t1", "t2"],
        false,
        RepeatMode::Off,
        0,
    )]);
    rig.director.start_playlist(&rig.pb, "pl").unwrap();
    rig.run(TRACK_MS * 3 + 1000);
    assert_eq!(rig.started(), vec![0, 1, 2]);
    assert_eq!(rig.state().status, PlaylistStatus::Finished);
    assert!(rig.pb.playing_sounds().is_empty());
    rig.run(TRACK_MS * 2);
    assert_eq!(rig.started(), vec![0, 1, 2], "nothing starts after the end");
}

#[test]
fn repeat_all_wraps_to_the_first_track() {
    let mut rig = Rig::new(3).with_playlists(vec![playlist(
        "pl",
        &["t0", "t1", "t2"],
        false,
        RepeatMode::All,
        0,
    )]);
    rig.director.start_playlist(&rig.pb, "pl").unwrap();
    rig.run(TRACK_MS * 4);
    assert_eq!(rig.started(), vec![0, 1, 2, 0, 1]);
    assert_eq!(rig.state().status, PlaylistStatus::Playing);
}

#[test]
fn repeat_one_loops_the_track_in_the_mixer() {
    let mut rig = Rig::new(2).with_playlists(vec![playlist(
        "pl",
        &["t0", "t1"],
        false,
        RepeatMode::One,
        3000,
    )]);
    rig.director.start_playlist(&rig.pb, "pl").unwrap();
    rig.run(TRACK_MS * 3);
    assert_eq!(rig.started(), vec![0], "looping needs no restart");
    assert!(rig.pb.is_looped(rig.sounds[0]));
    assert_eq!(rig.state().track.as_deref(), Some("t0"));
    // Skipping moves on, and the new track loops.
    rig.director
        .playlist_command(&rig.pb, PlaylistCommand::Next)
        .unwrap();
    assert_eq!(rig.started(), vec![0, 1]);
    assert!(rig.pb.is_looped(rig.sounds[1]));
    // Changing the mode applies to the track that plays.
    rig.director
        .playlist_command(&rig.pb, PlaylistCommand::SetRepeat(RepeatMode::All))
        .unwrap();
    assert!(!rig.pb.is_looped(rig.sounds[1]));
    rig.run(TRACK_MS);
    assert_eq!(rig.started().len(), 3, "t1 ended and the list wrapped");
}

// ---------------------------------------------------------- crossfade ----

#[test]
fn next_track_starts_crossfade_ms_before_the_end() {
    let mut rig = Rig::new(2).with_playlists(vec![playlist(
        "pl",
        &["t0", "t1"],
        false,
        RepeatMode::All,
        2000,
    )]);
    rig.director.start_playlist(&rig.pb, "pl").unwrap();
    rig.run(TRACK_MS - 2000 - STEP_MS);
    assert_eq!(rig.started(), vec![0], "too early");
    rig.run(STEP_MS);
    assert_eq!(rig.started(), vec![0, 1], "at duration - crossfade");

    let calls = rig.pb.calls();
    assert!(calls.iter().any(|c| matches!(
        c,
        MockCall::Play { sound, fade_ms: 2000, .. } if *sound == rig.sounds[1]
    )));
    assert!(calls.iter().any(|c| matches!(
        c,
        MockCall::Stop { sound, fade_ms: 2000 } if *sound == rig.sounds[0]
    )));

    // The new track fades in while the old one fades out.
    rig.run(1000);
    let (old, new) = (rig.pb.gain(rig.sounds[0]), rig.pb.gain(rig.sounds[1]));
    assert!(old > 0.0 && old < 1.0, "old {old}");
    assert!(new > 0.0 && new < 1.0, "new {new}");
    rig.run(1000);
    assert_eq!(rig.pb.gain(rig.sounds[0]), 0.0);
    assert_eq!(rig.pb.gain(rig.sounds[1]), 1.0);
    assert_eq!(rig.pb.track_count(rig.sounds[0]), 0, "the tail is gone");
    assert_eq!(rig.state().track.as_deref(), Some("t1"));
}

#[test]
fn a_late_tick_shortens_the_overlap_so_the_fade_ends_with_the_track() {
    let mut rig = Rig::new(2).with_playlists(vec![playlist(
        "pl",
        &["t0", "t1"],
        false,
        RepeatMode::All,
        2000,
    )]);
    rig.director.start_playlist(&rig.pb, "pl").unwrap();
    rig.pb.advance(9_000); // 1 s left, nobody ticked
    rig.tick();
    assert_eq!(rig.started(), vec![0, 1]);
    assert!(rig.pb.calls().iter().any(|c| matches!(
        c,
        MockCall::Play { sound, fade_ms: 1000, .. } if *sound == rig.sounds[1]
    )));
}

#[test]
fn a_track_shorter_than_twice_the_crossfade_overlaps_by_half() {
    let pb = MusicMock::new();
    let a = pb.add_track(3_000);
    let b = pb.add_track(3_000);
    let mut director = Director::new();
    director.sync_library(
        Vec::new(),
        vec![CatalogSound::new("a", a), CatalogSound::new("b", b)],
    );
    director.set_playlists(vec![playlist(
        "pl",
        &["a", "b"],
        false,
        RepeatMode::All,
        5000,
    )]);
    director.start_playlist(&pb, "pl").unwrap();
    pb.advance(1_400);
    director.tick_at(&pb, pb.now());
    assert_eq!(pb.starts_of(b), 0);
    pb.advance(100);
    director.tick_at(&pb, pb.now());
    assert_eq!(pb.starts_of(b), 1, "at half of the track");
}

#[test]
fn the_deadline_points_at_the_crossfade_point() {
    let mut rig = Rig::new(2).with_playlists(vec![playlist(
        "pl",
        &["t0", "t1"],
        false,
        RepeatMode::All,
        2000,
    )]);
    rig.director.start_playlist(&rig.pb, "pl").unwrap();
    let now = rig.pb.now();
    rig.director.tick_at(&rig.pb, now);
    let due = rig.director.next_deadline().expect("a deadline");
    let wait = due.saturating_duration_since(now).as_millis() as u64;
    assert_eq!(wait, TRACK_MS - 2000);
}

#[test]
fn a_single_track_playlist_crossfades_into_itself() {
    let mut rig =
        Rig::new(1).with_playlists(vec![playlist("pl", &["t0"], false, RepeatMode::All, 2000)]);
    rig.director.start_playlist(&rig.pb, "pl").unwrap();
    rig.run(TRACK_MS - 2000);
    assert_eq!(rig.started(), vec![0, 0]);
    assert!(rig.pb.is_playing(rig.sounds[0]), "the new run plays");
    rig.run(2500);
    assert_eq!(
        rig.pb.track_count(rig.sounds[0]),
        1,
        "only the new run is left"
    );
}

// ------------------------------------------------------------ controls ----

#[test]
fn next_and_prev_move_with_a_crossfade() {
    let mut rig = Rig::new(3).with_playlists(vec![playlist(
        "pl",
        &["t0", "t1", "t2"],
        false,
        RepeatMode::All,
        1500,
    )]);
    rig.director.start_playlist(&rig.pb, "pl").unwrap();
    rig.run(1000);
    rig.director
        .playlist_command(&rig.pb, PlaylistCommand::Next)
        .unwrap();
    assert_eq!(rig.started(), vec![0, 1]);
    assert!(rig.pb.calls().iter().any(|c| matches!(
        c,
        MockCall::Stop { sound, fade_ms: 1500 } if *sound == rig.sounds[0]
    )));
    // Within the first seconds: back to the previous track.
    rig.run(1000);
    rig.director
        .playlist_command(&rig.pb, PlaylistCommand::Prev)
        .unwrap();
    assert_eq!(rig.started(), vec![0, 1, 0]);
    // Later in the track: restart it instead.
    rig.run(5000);
    rig.director
        .playlist_command(&rig.pb, PlaylistCommand::Prev)
        .unwrap();
    assert_eq!(rig.started(), vec![0, 1, 0], "restarted by seeking");
    assert!(rig.state().position_ms < 1000);
    // Previous of the first track wraps to the last one (order, repeat all).
    rig.run(1000);
    rig.director
        .playlist_command(&rig.pb, PlaylistCommand::Prev)
        .unwrap();
    assert_eq!(rig.started().last(), Some(&2));
}

#[test]
fn next_at_the_end_of_a_repeat_off_list_stops() {
    let mut rig = Rig::new(2).with_playlists(vec![playlist(
        "pl",
        &["t0", "t1"],
        false,
        RepeatMode::Off,
        0,
    )]);
    rig.director.start_playlist(&rig.pb, "pl").unwrap();
    rig.director
        .playlist_command(&rig.pb, PlaylistCommand::Next)
        .unwrap();
    rig.director
        .playlist_command(&rig.pb, PlaylistCommand::Next)
        .unwrap();
    assert_eq!(rig.state().status, PlaylistStatus::Finished);
    assert!(rig.pb.playing_sounds().is_empty());
}

#[test]
fn seek_moves_the_position_and_is_clamped() {
    let mut rig = Rig::new(2).with_playlists(vec![playlist(
        "pl",
        &["t0", "t1"],
        false,
        RepeatMode::All,
        0,
    )]);
    rig.director.start_playlist(&rig.pb, "pl").unwrap();
    rig.director
        .playlist_command(&rig.pb, PlaylistCommand::Seek(4_000))
        .unwrap();
    rig.tick();
    assert_eq!(rig.state().position_ms, 4_000);
    assert_eq!(rig.state().duration_ms, Some(TRACK_MS));
    rig.director
        .playlist_command(&rig.pb, PlaylistCommand::Seek(999_999))
        .unwrap();
    assert!(rig.state().position_ms < TRACK_MS);
    assert!(rig.state().position_ms + 1 >= TRACK_MS);
}

#[test]
fn pause_freezes_the_position_and_the_transition_clock() {
    let mut rig = Rig::new(2).with_playlists(vec![playlist(
        "pl",
        &["t0", "t1"],
        false,
        RepeatMode::All,
        0,
    )]);
    rig.director.start_playlist(&rig.pb, "pl").unwrap();
    rig.run(3_000);
    rig.director
        .playlist_command(&rig.pb, PlaylistCommand::Pause)
        .unwrap();
    assert_eq!(rig.state().status, PlaylistStatus::Paused);
    rig.run(30_000);
    assert_eq!(rig.started(), vec![0], "a paused list does not advance");
    assert_eq!(rig.state().position_ms, 3_000);
    rig.director
        .playlist_command(&rig.pb, PlaylistCommand::Resume)
        .unwrap();
    assert_eq!(rig.state().status, PlaylistStatus::Playing);
    rig.run(7_000);
    assert_eq!(rig.started(), vec![0, 1], "7 s left at the pause");
    // Toggle round trip.
    rig.director
        .playlist_command(&rig.pb, PlaylistCommand::TogglePause)
        .unwrap();
    assert_eq!(rig.state().status, PlaylistStatus::Paused);
    rig.director
        .playlist_command(&rig.pb, PlaylistCommand::TogglePause)
        .unwrap();
    assert_eq!(rig.state().status, PlaylistStatus::Playing);
}

#[test]
fn commands_without_a_playlist_are_errors_not_panics() {
    let mut rig =
        Rig::new(1).with_playlists(vec![playlist("pl", &["t0"], false, RepeatMode::All, 0)]);
    for command in [
        PlaylistCommand::Next,
        PlaylistCommand::Prev,
        PlaylistCommand::Seek(5),
        PlaylistCommand::Pause,
        PlaylistCommand::Resume,
        PlaylistCommand::Start("nope".into()),
    ] {
        assert!(
            rig.director
                .playlist_command(&rig.pb, command.clone())
                .is_err(),
            "{command:?}"
        );
    }
    rig.director
        .playlist_command(&rig.pb, PlaylistCommand::Stop)
        .unwrap();
    rig.director
        .playlist_command(&rig.pb, PlaylistCommand::SetShuffle(true))
        .unwrap();
}

#[test]
fn an_output_without_music_support_still_plays_and_refuses_pause_and_seek() {
    let pb = MusicMock::without_music_support();
    let a = pb.add_track(5_000);
    let b = pb.add_track(5_000);
    let mut director = Director::new();
    director.sync_library(
        Vec::new(),
        vec![CatalogSound::new("a", a), CatalogSound::new("b", b)],
    );
    director.set_playlists(vec![playlist("pl", &["a", "b"], false, RepeatMode::All, 0)]);
    director.start_playlist(&pb, "pl").unwrap();
    assert!(director
        .playlist_command(&pb, PlaylistCommand::Pause)
        .is_err());
    assert!(director
        .playlist_command(&pb, PlaylistCommand::Seek(1000))
        .is_err());
    pb.advance(5_000);
    director.tick_at(&pb, pb.now());
    assert_eq!(pb.starts_of(b), 1, "advanced by the sound ending");
}

#[test]
fn a_track_stopped_from_outside_stops_the_playlist() {
    let mut rig = Rig::new(2).with_playlists(vec![playlist(
        "pl",
        &["t0", "t1"],
        false,
        RepeatMode::All,
        0,
    )]);
    rig.director.start_playlist(&rig.pb, "pl").unwrap();
    rig.run(2_000);
    rig.pb
        .stop(rig.sounds[0], soundshared::audio::FadeSpec::NONE)
        .unwrap();
    rig.run(30_000);
    assert_eq!(rig.started(), vec![0]);
    assert_eq!(rig.state().status, PlaylistStatus::Stopped);
}

// ----------------------------------------------------------- metadata ----

#[test]
fn state_carries_title_and_artist_of_the_current_track() {
    let pb = MusicMock::new();
    let a = pb.add_track_with_meta(
        5_000,
        TrackMeta {
            title: Some("Tavern Song".into()),
            artist: Some("The Bards".into()),
            album: None,
        },
    );
    let b = pb.add_track(5_000);
    let mut director = Director::new();
    director.sync_library(
        Vec::new(),
        vec![CatalogSound::new("a", a), CatalogSound::new("b", b)],
    );
    director.set_playlists(vec![playlist("pl", &["a", "b"], false, RepeatMode::All, 0)]);
    let watcher = director.subscribe_playlist();
    director.start_playlist(&pb, "pl").unwrap();
    let state = director.playlist_state();
    assert_eq!(state.title.as_deref(), Some("Tavern Song"));
    assert_eq!(state.artist.as_deref(), Some("The Bards"));
    assert_eq!(state.label("a.mp3"), "The Bards - Tavern Song");
    assert_eq!(state.track_index, Some(0));
    assert_eq!(state.track_count, 2);
    assert_eq!(state.name.as_deref(), Some("pl"));
    assert_eq!(*watcher.borrow(), state, "published to subscribers");

    director
        .playlist_command(&pb, PlaylistCommand::Next)
        .unwrap();
    let state = director.playlist_state();
    assert_eq!(state.title, None);
    assert_eq!(state.label("b.mp3"), "b.mp3");
    assert_eq!(watcher.borrow().track.as_deref(), Some("b"));
}

// ------------------------------------------------------------- scenes ----

fn scene_rig() -> Rig {
    Rig::new(4).with_scenes(
        vec![
            scene("camp", Some("calm"), true),
            scene("camp2", Some("calm"), true),
            scene("fight", Some("war"), true),
            scene("silent", None, true),
            scene("silent_keep", None, false),
        ],
        vec![
            playlist("calm", &["t0", "t1"], false, RepeatMode::All, 3000),
            playlist("war", &["t2", "t3"], false, RepeatMode::All, 500),
        ],
    )
}

#[test]
fn activating_a_scene_starts_its_playlist_with_a_fade_in() {
    let mut rig = scene_rig();
    let report = rig.director.activate_scene(&rig.pb, "camp").unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(rig.started(), vec![0]);
    // max(scene fade-in 1000, crossfade 3000)
    assert!(rig.pb.calls().iter().any(|c| matches!(
        c,
        MockCall::Play { sound, fade_ms: 3000, .. } if *sound == rig.sounds[0]
    )));
    assert_eq!(rig.pb.bus_of(rig.sounds[0]), Some(Bus::Music));
    assert_eq!(rig.state().playlist.as_deref(), Some("calm"));
    assert_eq!(rig.state().status, PlaylistStatus::Playing);
}

#[test]
fn a_scene_with_the_same_playlist_does_not_restart_it() {
    let mut rig = scene_rig();
    rig.director.activate_scene(&rig.pb, "camp").unwrap();
    rig.run(5_000);
    rig.pb.inner().clear_calls();
    rig.director.activate_scene(&rig.pb, "camp2").unwrap();
    rig.tick();
    assert_eq!(rig.started(), vec![0], "no restart");
    assert_eq!(rig.state().position_ms, 5_000, "no jump");
    assert!(
        !rig.pb
            .calls()
            .iter()
            .any(|c| matches!(c, MockCall::Stop { sound, .. } if *sound == rig.sounds[0])),
        "the track is not faded out by the other scene's stop_others"
    );
    assert!(rig.pb.is_playing(rig.sounds[0]));
}

#[test]
fn a_scene_with_another_playlist_replaces_the_music() {
    let mut rig = scene_rig();
    rig.director.activate_scene(&rig.pb, "camp").unwrap();
    rig.run(4_000);
    let report = rig.director.activate_scene(&rig.pb, "fight").unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(rig.started(), vec![0, 2]);
    assert_eq!(rig.state().playlist.as_deref(), Some("war"));
    assert!(!rig.pb.is_playing(rig.sounds[0]), "the old track fades out");
    rig.run(2_500);
    assert_eq!(rig.pb.track_count(rig.sounds[0]), 0, "no tail left");
    assert!(rig.pb.gain(rig.sounds[2]) > 0.99);
    // And it keeps playing (the old playlist does not advance by itself).
    rig.run(TRACK_MS * 2);
    let order = rig.started();
    assert!(order.iter().all(|i| *i >= 2 || *i == 0), "{order:?}");
    assert_eq!(order.iter().filter(|i| **i == 1).count(), 0, "{order:?}");
}

#[test]
fn a_scene_without_a_playlist_fades_out_its_predecessors_music() {
    let mut rig = scene_rig();
    rig.director.activate_scene(&rig.pb, "camp").unwrap();
    rig.run(4_000);
    rig.director.activate_scene(&rig.pb, "silent_keep").unwrap();
    assert_eq!(rig.state().status, PlaylistStatus::Stopped);
    rig.run(2_500);
    assert_eq!(rig.pb.track_count(rig.sounds[0]), 0);
    rig.run(TRACK_MS * 3);
    assert_eq!(rig.started(), vec![0], "a stopped playlist stays stopped");
}

#[test]
fn stopping_the_scene_stops_its_music() {
    let mut rig = scene_rig();
    rig.director.activate_scene(&rig.pb, "camp").unwrap();
    rig.run(1_000);
    rig.director.stop_scene(&rig.pb).unwrap();
    assert_eq!(rig.state().status, PlaylistStatus::Stopped);
    assert!(rig.pb.playing_sounds().is_empty());
}

#[test]
fn stop_all_stops_the_playlist_and_it_does_not_come_back() {
    let mut rig = scene_rig();
    rig.director.activate_scene(&rig.pb, "camp").unwrap();
    rig.run(1_000);
    rig.director.stop_all(&rig.pb).unwrap();
    rig.run(TRACK_MS * 3);
    assert_eq!(rig.started(), vec![0]);
    assert_eq!(rig.state().status, PlaylistStatus::Stopped);
}

#[test]
fn a_hand_started_playlist_survives_a_scene_that_keeps_others() {
    let mut rig = scene_rig();
    rig.director.start_playlist(&rig.pb, "calm").unwrap();
    rig.run(1_000);
    rig.director.activate_scene(&rig.pb, "silent_keep").unwrap();
    assert_eq!(rig.state().status, PlaylistStatus::Playing);
    assert!(rig.pb.is_playing(rig.sounds[0]));
}

#[test]
fn a_stop_others_scene_takes_a_hand_started_playlist_and_it_does_not_advance() {
    let mut rig = scene_rig();
    rig.director.start_playlist(&rig.pb, "calm").unwrap();
    rig.run(1_000);
    rig.director.activate_scene(&rig.pb, "silent").unwrap();
    rig.run(TRACK_MS * 3);
    assert_eq!(rig.started(), vec![0]);
    assert_eq!(rig.state().status, PlaylistStatus::Stopped);
}

#[test]
fn reactivating_after_a_finished_list_starts_it_again() {
    let mut rig = Rig::new(1).with_scenes(
        vec![scene("s", Some("once"), true)],
        vec![playlist("once", &["t0"], false, RepeatMode::Off, 0)],
    );
    rig.director.activate_scene(&rig.pb, "s").unwrap();
    rig.run(TRACK_MS + 500);
    assert_eq!(rig.state().status, PlaylistStatus::Finished);
    rig.director.activate_scene(&rig.pb, "s").unwrap();
    assert_eq!(rig.started(), vec![0, 0]);
    assert_eq!(rig.state().status, PlaylistStatus::Playing);
}

// ------------------------------------------------- broken playlists ----

#[test]
fn an_empty_playlist_does_not_panic_and_reports() {
    let mut rig = Rig::new(1).with_scenes(
        vec![scene("s", Some("empty"), true)],
        vec![playlist("empty", &[], true, RepeatMode::All, 1000)],
    );
    assert!(rig.director.start_playlist(&rig.pb, "empty").is_err());
    let report = rig.director.activate_scene(&rig.pb, "s").unwrap();
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    rig.run(5_000);
    assert_eq!(rig.state().status, PlaylistStatus::Stopped);
    assert_eq!(rig.state().track_count, 0);
    assert!(rig.state().error.is_some());
}

#[test]
fn a_scene_naming_an_unknown_playlist_reports_it() {
    let mut rig = Rig::new(1).with_scenes(vec![scene("s", Some("ghost"), true)], vec![]);
    let report = rig.director.activate_scene(&rig.pb, "s").unwrap();
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].contains("ghost"));
    assert_eq!(rig.director.active_scene(), Some("s"));
}

#[test]
fn missing_and_failing_tracks_are_skipped() {
    let mut rig = Rig::new(3).with_playlists(vec![playlist(
        "pl",
        &["gone", "t0", "t1", "t2"],
        false,
        RepeatMode::All,
        0,
    )]);
    rig.pb.fail_on(rig.sounds[1]);
    rig.director.start_playlist(&rig.pb, "pl").unwrap();
    // "gone" is not in the library, t1's file fails: t0 plays first ...
    assert_eq!(rig.started(), vec![0]);
    assert!(rig.state().error.is_some(), "the skipped item is reported");
    // ... and after it the failing one is passed over.
    rig.run(TRACK_MS);
    assert_eq!(rig.started(), vec![0, 2]);
    assert_eq!(rig.state().status, PlaylistStatus::Playing);
}

#[test]
fn a_playlist_with_nothing_playable_stops_instead_of_looping_forever() {
    let mut rig = Rig::new(2).with_playlists(vec![playlist(
        "pl",
        &["gone", "t0", "t1"],
        true,
        RepeatMode::All,
        2000,
    )]);
    rig.pb.fail_on(rig.sounds[0]);
    rig.pb.fail_on(rig.sounds[1]);
    assert!(rig.director.start_playlist(&rig.pb, "pl").is_err());
    assert_eq!(rig.state().status, PlaylistStatus::Stopped);
    rig.run(10_000);
    assert!(rig.started().is_empty());
}

#[test]
fn a_track_that_fails_midway_through_a_list_falls_through_to_the_rest() {
    let mut rig = Rig::new(3).with_playlists(vec![playlist(
        "pl",
        &["t0", "t1", "t2"],
        false,
        RepeatMode::Off,
        0,
    )]);
    rig.director.start_playlist(&rig.pb, "pl").unwrap();
    rig.pb.fail_on(rig.sounds[1]);
    rig.pb.fail_on(rig.sounds[2]);
    rig.run(TRACK_MS + 500);
    // Nothing after t0 can start and repeat is off: the list is over.
    assert_eq!(rig.started(), vec![0]);
    assert_ne!(rig.state().status, PlaylistStatus::Playing);
}

// ----------------------------------------------------- library edits ----

#[test]
fn a_library_resync_keeps_runtime_shuffle_and_repeat_but_applies_edits() {
    let base = playlist("pl", &["t0", "t1", "t2"], false, RepeatMode::All, 0);
    let mut rig = Rig::new(4).with_playlists(vec![base.clone()]);
    rig.director.start_playlist(&rig.pb, "pl").unwrap();
    rig.director
        .playlist_command(&rig.pb, PlaylistCommand::SetRepeat(RepeatMode::One))
        .unwrap();
    // The GUI re-syncs an unchanged library before every command.
    rig.director.set_playlists(vec![base.clone()]);
    rig.tick();
    assert_eq!(rig.state().repeat, RepeatMode::One);

    // An edit that adds a track is picked up; the current track plays on.
    let mut edited = base.clone();
    edited.items.push("t3".into());
    edited.repeat = RepeatMode::Off;
    rig.director.set_playlists(vec![edited]);
    rig.tick();
    assert_eq!(rig.state().track_count, 4);
    assert_eq!(rig.state().repeat, RepeatMode::Off);
    assert_eq!(rig.started(), vec![0]);
    rig.run(TRACK_MS * 3);
    assert_eq!(rig.started(), vec![0, 1, 2, 3]);
}

#[test]
fn removing_the_playing_track_from_the_list_lets_it_finish() {
    let mut rig = Rig::new(3).with_playlists(vec![playlist(
        "pl",
        &["t0", "t1", "t2"],
        false,
        RepeatMode::All,
        0,
    )]);
    rig.director.start_playlist(&rig.pb, "pl").unwrap();
    rig.run(1_000);
    rig.director.set_playlists(vec![playlist(
        "pl",
        &["t1", "t2"],
        false,
        RepeatMode::All,
        0,
    )]);
    rig.tick();
    assert!(rig.pb.is_playing(rig.sounds[0]));
    rig.run(TRACK_MS);
    assert_eq!(
        rig.started()[1],
        1,
        "continues from the start of the new list"
    );
}

// ------------------------------------------------------------ commands ----

#[test]
fn the_generic_director_command_drives_the_playlist() {
    let mut rig = Rig::new(2).with_playlists(vec![playlist(
        "pl",
        &["t0", "t1"],
        false,
        RepeatMode::All,
        0,
    )]);
    for command in [
        PlaylistCommand::Start("pl".into()),
        PlaylistCommand::Next,
        PlaylistCommand::SetShuffle(true),
        PlaylistCommand::Stop,
    ] {
        rig.director
            .execute(&rig.pb, DirectorCommand::Playlist(command))
            .unwrap();
    }
    assert_eq!(rig.state().status, PlaylistStatus::Stopped);
    assert!(rig.state().shuffle);
}

#[test]
fn audio_commands_map_onto_playlist_commands() {
    use soundshared::gui::types::AudioCommand;
    let (tx, _rx) = crossbeam_channel::bounded(1);
    let cases = vec![
        (
            AudioCommand::PlaylistNext {
                response: tx.clone(),
            },
            PlaylistCommand::Next,
        ),
        (
            AudioCommand::PlaylistPrev {
                response: tx.clone(),
            },
            PlaylistCommand::Prev,
        ),
        (
            AudioCommand::PlaylistSeek {
                ms: 5,
                response: tx.clone(),
            },
            PlaylistCommand::Seek(5),
        ),
        (
            AudioCommand::PlaylistPause {
                response: tx.clone(),
            },
            PlaylistCommand::Pause,
        ),
        (
            AudioCommand::PlaylistResume {
                response: tx.clone(),
            },
            PlaylistCommand::Resume,
        ),
        (
            AudioCommand::PlaylistSetShuffle {
                shuffle: true,
                response: tx.clone(),
            },
            PlaylistCommand::SetShuffle(true),
        ),
        (
            AudioCommand::PlaylistSetRepeat {
                repeat: RepeatMode::One,
                response: tx.clone(),
            },
            PlaylistCommand::SetRepeat(RepeatMode::One),
        ),
    ];
    for (audio, expected) in cases {
        let (command, _) = audio.into_playlist().expect("a playlist command");
        assert_eq!(command, expected);
    }
    assert!(AudioCommand::StopScene { response: tx }
        .into_playlist()
        .is_err());
}

#[test]
fn the_artificial_clock_is_monotonic() {
    let rig = Rig::new(1);
    let a: Instant = rig.pb.now();
    rig.pb.advance(10);
    assert!(rig.pb.now() > a);
}

// ------------------------------------------------------- real engine ----

/// A 48 kHz stereo 16-bit WAV of `frames` frames with an INFO title.
fn write_test_wav(path: &std::path::Path, frames: usize, title: &str) {
    let mut value = title.as_bytes().to_vec();
    value.push(0);
    if value.len() % 2 == 1 {
        value.push(0);
    }
    let mut list = b"INFO".to_vec();
    list.extend_from_slice(b"INAM");
    list.extend_from_slice(&(value.len() as u32).to_le_bytes());
    list.extend_from_slice(&value);
    let data_len = (frames * 4) as u32;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(4 + 24 + 8 + data_len + 8 + list.len() as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&48_000u32.to_le_bytes());
    bytes.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"LIST");
    bytes.extend_from_slice(&(list.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&list);
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        let s = (((i % 100) as i16) - 50) * 100;
        bytes.extend_from_slice(&s.to_le_bytes());
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

/// Two streamed files through the real engine (headless output): the playlist
/// crossfades from the first into the second off the engine's own position
/// and end events, and finishes. Real time, about three seconds.
#[test]
fn a_playlist_plays_through_the_real_engine() {
    use soundshared::audio::AudioEngine;

    let dir = std::env::temp_dir().join(format!(
        "bardwave-playlist-test-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let first = dir.join("first.wav");
    let second = dir.join("second.wav");
    write_test_wav(&first, 48_000 * 3 / 2, "First Song");
    write_test_wav(&second, 48_000, "Second Song");

    let engine = AudioEngine::new().expect("engine");
    let a = engine.load_sound_streaming(&first).unwrap();
    let b = engine.load_sound_streaming(&second).unwrap();
    engine.start_headless_output().unwrap();

    let mut director = Director::new();
    director.sync_library(
        Vec::new(),
        vec![CatalogSound::new("a", a), CatalogSound::new("b", b)],
    );
    director.set_playlists(vec![playlist(
        "pl",
        &["a", "b"],
        false,
        RepeatMode::Off,
        400,
    )]);
    director.start_playlist(&engine, "pl").unwrap();
    let state = director.playlist_state();
    assert_eq!(state.title.as_deref(), Some("First Song"));
    assert_eq!(state.duration_ms, Some(1_500));

    let mut seen_second = false;
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    while Instant::now() < deadline {
        director.tick(&engine);
        let state = director.playlist_state();
        if state.track.as_deref() == Some("b") {
            seen_second = true;
            assert_eq!(state.title.as_deref(), Some("Second Song"));
        }
        if state.status == PlaylistStatus::Finished {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(seen_second, "the second track never started");
    assert_eq!(director.playlist_state().status, PlaylistStatus::Finished);

    // Seek and pause on the real engine.
    director.start_playlist(&engine, "pl").unwrap();
    director
        .playlist_command(&engine, PlaylistCommand::Seek(1_000))
        .unwrap();
    director
        .playlist_command(&engine, PlaylistCommand::Pause)
        .unwrap();
    let paused_at = director.playlist_state().position_ms;
    assert!(paused_at >= 1_000, "{paused_at}");
    std::thread::sleep(std::time::Duration::from_millis(300));
    director.tick(&engine);
    assert_eq!(director.playlist_state().status, PlaylistStatus::Paused);
    director
        .playlist_command(&engine, PlaylistCommand::Resume)
        .unwrap();
    director.stop_all(&engine).unwrap();
    engine.stop_output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

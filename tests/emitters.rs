//! Emitters (random / periodic sounds of a scene) through the Director on a
//! mock playback: start with the scene, stop with it, overlap, determinism.
//!
//! No sound card, no network, no config files: time is an explicit `Instant`
//! offset, so the tests do not sleep.

use std::time::{Duration, Instant};

use soundshared::audio::director::mock::{MockCall, MockPlayback};
use soundshared::audio::{CatalogSound, Director, Playback, SoundId};
use soundshared::config::{EmitterConfig, SceneConfig, SceneItemConfig, SceneStart};

struct Rig {
    pb: MockPlayback,
    director: Director,
    base: Instant,
    crow: SoundId,
    drip: SoundId,
    rain: SoundId,
}

fn emitter(sounds: &[&str], min: u32, max: u32) -> EmitterConfig {
    EmitterConfig {
        sounds: sounds.iter().map(|s| s.to_string()).collect(),
        min_interval_ms: min,
        max_interval_ms: max,
        ..Default::default()
    }
}

fn scene(id: &str, emitters: Vec<EmitterConfig>) -> SceneConfig {
    SceneConfig {
        id: id.into(),
        name: id.into(),
        fade_in_ms: 500,
        fade_out_ms: 1500,
        items: vec![SceneItemConfig {
            sound: "rain".into(),
            volume: 0.5,
            looped: true,
            start: SceneStart::OnEnter,
            delay_ms: 0,
            pan: 0.0,
        }],
        emitters,
        ..Default::default()
    }
}

fn rig_with(scenes: Vec<SceneConfig>, seed: u64) -> Rig {
    let pb = MockPlayback::new();
    let (crow, drip, rain) = (pb.add_sound(), pb.add_sound(), pb.add_sound());
    pb.set_duration(crow, Some(2_000));
    pb.set_duration(drip, Some(500));
    let mut director = Director::new();
    director.set_emitter_seed(seed);
    director.sync_library(
        scenes,
        vec![
            CatalogSound::new("crow", crow),
            CatalogSound::new("drip", drip),
            CatalogSound::new("rain", rain),
        ],
    );
    Rig {
        pb,
        director,
        base: Instant::now(),
        crow,
        drip,
        rain,
    }
}

fn at(r: &Rig, ms: u64) -> Instant {
    r.base + Duration::from_millis(ms)
}

fn activate(r: &mut Rig, id: &str, ms: u64) {
    let now = at(r, ms);
    r.director.activate_scene_at(&r.pb, id, now).unwrap();
}

/// Tick every 50 ms from `from` to `to` (milliseconds after the base).
fn run(r: &mut Rig, from: u64, to: u64) {
    let mut t = from;
    while t <= to {
        let now = at(r, t);
        r.director.tick_at(&r.pb, now);
        t += 50;
    }
}

fn one_shots(r: &Rig) -> Vec<(SoundId, f32)> {
    r.pb.calls()
        .into_iter()
        .filter_map(|c| match c {
            MockCall::OneShot { sound, volume } => Some((sound, volume)),
            _ => None,
        })
        .collect()
}

fn stopped_with_fade(r: &Rig, sound: SoundId, fade: u32) -> bool {
    r.pb.calls().iter().any(
        |c| matches!(c, MockCall::Stop { sound: s, fade_ms } if *s == sound && *fade_ms == fade),
    )
}

#[test]
fn emitters_start_with_the_scene() {
    let scenes = vec![scene("forest", vec![emitter(&["crow"], 1_000, 2_000)])];
    let mut r = rig_with(scenes.clone(), 1);
    activate(&mut r, "forest", 0);
    assert!(r.pb.is_playing(r.rain));
    assert!(one_shots(&r).is_empty(), "the first play is shifted");
    run(&mut r, 50, 60_000);
    let plays = one_shots(&r);
    assert!(plays.len() >= 15, "{} plays in a minute", plays.len());
    assert!(plays.iter().all(|(s, _)| *s == r.crow));

    // The first play comes within the first shift.
    let mut r2 = rig_with(scenes, 1);
    activate(&mut r2, "forest", 0);
    let mut t = 0;
    while one_shots(&r2).is_empty() {
        t += 50;
        run(&mut r2, t, t);
        assert!(t <= 6_000);
    }
    assert!(t <= 5_050);
}

#[test]
fn scene_change_stops_emitters_and_fades_their_instances() {
    let mut r = rig_with(
        vec![
            scene("forest", vec![emitter(&["crow"], 200, 300)]),
            scene("cave", vec![]),
        ],
        2,
    );
    activate(&mut r, "forest", 0);
    run(&mut r, 50, 3_000);
    assert!(!one_shots(&r).is_empty());
    assert!(r.pb.is_playing(r.crow));
    assert!(r.director.next_deadline().is_some());

    r.pb.clear_calls();
    activate(&mut r, "cave", 3_000);
    // The crow that still sounded was faded out with the new scene's fade.
    assert!(stopped_with_fade(&r, r.crow, 1500));
    assert!(r.director.emitter_snapshot(at(&r, 3_000)).is_empty());
    assert_eq!(r.director.next_deadline(), None);

    r.pb.clear_calls();
    run(&mut r, 3_050, 120_000);
    assert!(one_shots(&r).is_empty(), "no play after the scene changed");
}

#[test]
fn stop_scene_fades_instances_with_the_scene_fade_out() {
    let mut r = rig_with(vec![scene("forest", vec![emitter(&["crow"], 200, 300)])], 3);
    activate(&mut r, "forest", 0);
    run(&mut r, 50, 2_000);
    assert!(r.pb.is_playing(r.crow));
    r.pb.clear_calls();
    r.director.stop_scene(&r.pb).unwrap();
    assert!(stopped_with_fade(&r, r.crow, 1500));
    assert_eq!(r.director.next_deadline(), None);
    assert!(r.director.emitter_snapshot(at(&r, 2_000)).is_empty());
    r.pb.clear_calls();
    run(&mut r, 2_050, 60_000);
    assert!(one_shots(&r).is_empty());
}

#[test]
fn stop_all_stops_emitters() {
    let mut r = rig_with(vec![scene("forest", vec![emitter(&["crow"], 200, 300)])], 4);
    activate(&mut r, "forest", 0);
    run(&mut r, 50, 2_000);
    r.director.stop_all_with_fade(&r.pb, 1_000).unwrap();
    r.pb.clear_calls();
    run(&mut r, 2_050, 60_000);
    assert!(one_shots(&r).is_empty());
    assert_eq!(r.director.next_deadline(), None);
}

#[test]
fn activating_the_active_scene_again_keeps_timers_and_instances() {
    let mut r = rig_with(
        vec![scene("forest", vec![emitter(&["crow"], 30_000, 40_000)])],
        5,
    );
    r.pb.set_duration(r.crow, Some(60_000));
    activate(&mut r, "forest", 0);
    let due_before = r.director.next_deadline().unwrap();
    activate(&mut r, "forest", 100);
    assert_eq!(r.director.next_deadline().unwrap(), due_before);

    // Sync with identical content does not reset either.
    let scenes = r.director.scenes().to_vec();
    let catalog = r.director.catalog().to_vec();
    r.director.sync_library(scenes, catalog);
    run(&mut r, 150, 150);
    assert_eq!(r.director.next_deadline().unwrap(), due_before);

    // Let one crow play, then re-activate: it must not be cut off.
    run(&mut r, 200, 6_000);
    assert!(r.pb.is_playing(r.crow));
    r.pb.clear_calls();
    activate(&mut r, "forest", 6_000);
    assert!(!r
        .pb
        .calls()
        .iter()
        .any(|c| matches!(c, MockCall::Stop { sound, .. } if *sound == r.crow)));
}

#[test]
fn overlap_limit_holds_through_the_director() {
    let mut cfg = emitter(&["crow"], 100, 300);
    cfg.max_overlap = 2;
    let mut r = rig_with(vec![scene("forest", vec![cfg])], 6);
    r.pb.set_duration(r.crow, Some(10_000));
    activate(&mut r, "forest", 0);
    // Record the time of each play by sampling the call count per tick.
    let mut times = Vec::new();
    let mut seen = 0;
    let mut t = 50;
    while t <= 120_000 {
        let now = at(&r, t);
        r.director.tick_at(&r.pb, now);
        let n = one_shots(&r).len();
        for _ in seen..n {
            times.push(t);
        }
        seen = n;
        t += 50;
    }
    assert!(times.len() > 4);
    for (i, start) in times.iter().enumerate() {
        let in_window = times[i..].iter().filter(|x| **x < start + 10_000).count();
        assert!(
            in_window <= 2,
            "{in_window} instances inside 10 s at {start}"
        );
    }
}

#[test]
fn same_seed_same_plays() {
    fn trace(seed: u64) -> Vec<(usize, i32)> {
        let mut cfg = emitter(&["crow", "drip"], 300, 1_500);
        cfg.volume = 0.6;
        cfg.volume_jitter = 0.5;
        cfg.pan_jitter = 0.7;
        cfg.max_overlap = 3;
        let mut r = rig_with(vec![scene("forest", vec![cfg])], seed);
        activate(&mut r, "forest", 0);
        run(&mut r, 50, 90_000);
        let ids = [r.crow, r.drip];
        one_shots(&r)
            .into_iter()
            .map(|(s, v)| {
                (
                    ids.iter().position(|i| *i == s).unwrap(),
                    (v * 1000.0).round() as i32,
                )
            })
            .collect()
    }
    let a = trace(99);
    assert!(a.len() > 20);
    assert_eq!(a, trace(99));
    assert_ne!(a, trace(100));
}

#[test]
fn levels_and_pans_stay_in_range() {
    let mut cfg = emitter(&["drip"], 100, 200);
    cfg.volume = 0.8;
    cfg.volume_jitter = 1.0;
    cfg.pan_jitter = 1.0;
    let mut r = rig_with(vec![scene("cave", vec![cfg])], 7);
    r.pb.set_duration(r.drip, Some(50));
    activate(&mut r, "cave", 0);
    let mut t = 50;
    let mut plays = 0;
    while t <= 60_000 {
        let before = one_shots(&r).len();
        let now = at(&r, t);
        r.director.tick_at(&r.pb, now);
        if one_shots(&r).len() > before {
            plays += 1;
            let pan = r.pb.pan_of(r.drip).unwrap();
            assert!((-1.0..=1.0).contains(&pan));
        }
        t += 50;
    }
    assert!(plays > 100);
    for (_, volume) in one_shots(&r) {
        assert!((0.0..=1.0).contains(&volume));
    }
}

#[test]
fn snapshot_reports_time_to_the_next_sound() {
    let mut r = rig_with(
        vec![scene(
            "forest",
            vec![
                emitter(&["crow"], 20_000, 30_000),
                emitter(&["unknown"], 1_000, 2_000), // not in the catalog
                emitter(&[], 1_000, 2_000),
            ],
        )],
        8,
    );
    assert!(r.director.emitter_snapshot(at(&r, 0)).is_empty());
    r.pb.set_duration(r.crow, Some(60_000));
    activate(&mut r, "forest", 0);
    let snap = r.director.emitter_snapshot(at(&r, 0));
    assert_eq!(snap.len(), 3);
    assert!(snap[0].enabled);
    assert!(snap[0].next_in_ms.unwrap() <= 5_000);
    assert!(!snap[1].enabled && snap[1].next_in_ms.is_none());
    assert!(!snap[2].enabled);
    // It counts down, and jumps by a full interval after the play.
    let later = r.director.emitter_snapshot(at(&r, 100));
    assert!(later[0].next_in_ms.unwrap() <= snap[0].next_in_ms.unwrap());
    run(&mut r, 50, 5_100);
    let after = r.director.emitter_snapshot(at(&r, 5_100));
    assert!((14_000..=30_000).contains(&after[0].next_in_ms.unwrap()));
    assert_eq!(after[0].active, 1);
}

#[test]
fn editing_the_active_scene_rebuilds_and_bad_emitters_are_harmless() {
    let mut r = rig_with(
        vec![scene("forest", vec![emitter(&["crow"], 30_000, 40_000)])],
        9,
    );
    activate(&mut r, "forest", 0);
    assert_eq!(r.director.emitter_snapshot(at(&r, 0)).len(), 1);

    // min > max, zero overlap, NaN level, empty set: nothing hangs or panics.
    let mut weird = emitter(&["drip"], 5_000, 1_000);
    weird.max_overlap = 0;
    weird.volume = f32::NAN;
    weird.pan_jitter = f32::NAN;
    let catalog = r.director.catalog().to_vec();
    r.director.sync_library(
        vec![scene("forest", vec![weird, emitter(&[], 0, 0)])],
        catalog,
    );
    run(&mut r, 50, 60_000);
    let snap = r.director.emitter_snapshot(at(&r, 60_000));
    assert_eq!(snap.len(), 2);
    assert!(snap[0].enabled && !snap[1].enabled);
    assert!(!one_shots(&r).is_empty());
    for (_, volume) in one_shots(&r) {
        assert!((0.0..=1.0).contains(&volume));
    }

    // The scene loses its emitters: they stop.
    let catalog = r.director.catalog().to_vec();
    r.director
        .sync_library(vec![scene("forest", vec![])], catalog);
    r.pb.clear_calls();
    run(&mut r, 60_050, 200_000);
    assert!(one_shots(&r).is_empty());
    assert_eq!(r.director.next_deadline(), None);
}

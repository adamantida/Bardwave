//! Session timers: the pure timeline, the Director actions on a mock playback,
//! the `[[timers]]` config format and the threaded runner.
//!
//! No sound card, no network, no real config: time is an explicit `now_ms`.

use std::time::{Duration, Instant};

use soundshared::audio::director::mock::{MockCall, MockPlayback};
use soundshared::audio::{
    CatalogSound, Director, Timeline, TimelineRunner, TimelineStatus, TimerEvent,
};
use soundshared::config::{
    Config, SceneConfig, SceneItemConfig, SceneStart, TimerAction, TimerConfig, TimerTrigger,
};

fn timer(id: &str, trigger: TimerTrigger, action: TimerAction) -> TimerConfig {
    TimerConfig {
        id: id.into(),
        name: id.into(),
        trigger,
        action,
        enabled: true,
    }
}

fn scene(id: &str, sound: &str) -> SceneConfig {
    SceneConfig {
        id: id.into(),
        name: id.into(),
        items: vec![SceneItemConfig {
            sound: sound.into(),
            volume: 1.0,
            looped: true,
            start: SceneStart::OnEnter,
            delay_ms: 0,
        }],
        ..Default::default()
    }
}

/// A Director with one scene ("alarm") and two sounds ("siren", "bell").
fn rig() -> (Director, MockPlayback) {
    let pb = MockPlayback::new();
    let siren = pb.add_sound();
    let bell = pb.add_sound();
    let mut director = Director::new();
    director.sync_library(
        vec![scene("alarm", "siren")],
        vec![
            CatalogSound::new("siren", siren),
            CatalogSound::new("bell", bell),
        ],
    );
    (director, pb)
}

fn run_all(director: &mut Director, pb: &MockPlayback, events: &[TimerEvent]) -> Vec<String> {
    events
        .iter()
        .flat_map(|event| director.run_timer_action(pb, &event.action).warnings)
        .collect()
}

#[test]
fn after_timer_activates_the_scene_once() {
    let (mut director, pb) = rig();
    let mut timeline = Timeline::new(vec![timer(
        "t",
        TimerTrigger::AfterMs(600_000),
        TimerAction::ActivateScene {
            scene: "alarm".into(),
        },
    )]);
    timeline.start(0);
    assert!(timeline.tick(599_900).is_empty());
    let events = timeline.tick(600_000);
    assert!(run_all(&mut director, &pb, &events).is_empty());
    assert_eq!(director.active_scene(), Some("alarm"));
    assert!(timeline.tick(1_200_000).is_empty());
}

#[test]
fn every_timer_plays_the_stinger_each_period() {
    let (mut director, pb) = rig();
    let mut timeline = Timeline::new(vec![timer(
        "bell",
        TimerTrigger::EveryMs(30 * 60_000),
        TimerAction::PlayStinger {
            sound: "bell".into(),
        },
    )]);
    timeline.start(0);
    let mut fired = 0;
    for minute in 1..=95u64 {
        let events = timeline.tick(minute * 60_000);
        fired += events.len();
        assert!(run_all(&mut director, &pb, &events).is_empty());
    }
    assert_eq!(fired, 3);
    let one_shots = pb
        .calls()
        .iter()
        .filter(|call| matches!(call, MockCall::OneShot { .. }))
        .count();
    assert_eq!(one_shots, 3);
}

#[test]
fn paused_time_is_not_counted() {
    let mut timeline = Timeline::new(vec![timer(
        "t",
        TimerTrigger::AfterMs(10_000),
        TimerAction::StopScene,
    )]);
    timeline.start(0);
    timeline.pause(4_000);
    // Pausing for a long time, with the thread still ticking.
    for now in (5_000..=100_000).step_by(5_000) {
        assert!(timeline.tick(now).is_empty());
    }
    timeline.start(100_000);
    assert!(timeline.tick(105_900).is_empty());
    assert_eq!(timeline.tick(106_000).len(), 1);
}

#[test]
fn reset_stops_everything() {
    let mut timeline = Timeline::new(vec![timer(
        "t",
        TimerTrigger::EveryMs(1_000),
        TimerAction::StopScene,
    )]);
    timeline.start(0);
    assert_eq!(timeline.tick(1_000).len(), 1);
    timeline.reset();
    assert_eq!(timeline.status(), TimelineStatus::Stopped);
    assert!(timeline.tick(50_000).is_empty());
    timeline.start(50_000);
    assert_eq!(timeline.tick(51_000).len(), 1);
}

#[test]
fn disabled_timer_stays_silent() {
    let mut off = timer(
        "off",
        TimerTrigger::AfterMs(100),
        TimerAction::StopAll { fade_ms: 0 },
    );
    off.enabled = false;
    let mut timeline = Timeline::new(vec![off]);
    timeline.start(0);
    assert!(timeline.tick(60_000).is_empty());
}

#[test]
fn unknown_scene_warns_and_changes_nothing() {
    let (mut director, pb) = rig();
    pb.clear_calls();
    let run = director.run_timer_action(
        &pb,
        &TimerAction::ActivateScene {
            scene: "deleted".into(),
        },
    );
    assert_eq!(run.warnings.len(), 1);
    assert!(run.warnings[0].contains("deleted"), "{:?}", run.warnings);
    assert_eq!(director.active_scene(), None);
    assert!(pb.calls().is_empty());
}

#[test]
fn unknown_sound_and_playlist_warn_without_panicking() {
    let (mut director, pb) = rig();
    let sound = director.run_timer_action(
        &pb,
        &TimerAction::PlayStinger {
            sound: "gone".into(),
        },
    );
    assert!(!sound.is_clean());
    let playlist = director.run_timer_action(
        &pb,
        &TimerAction::StartPlaylist {
            playlist: "gone".into(),
        },
    );
    assert!(!playlist.is_clean());
    // Music controls with nothing playing are warnings too, never panics.
    for action in [
        TimerAction::NextTrack,
        TimerAction::PauseMusic,
        TimerAction::ResumeMusic,
        TimerAction::StopPlaylist,
    ] {
        let _ = director.run_timer_action(&pb, &action);
    }
}

#[test]
fn a_bad_timer_does_not_stop_the_others_in_the_same_tick() {
    let (mut director, pb) = rig();
    let mut timeline = Timeline::new(vec![
        timer(
            "bad",
            TimerTrigger::AfterMs(1_000),
            TimerAction::ActivateScene {
                scene: "deleted".into(),
            },
        ),
        timer(
            "good",
            TimerTrigger::AfterMs(1_000),
            TimerAction::ActivateScene {
                scene: "alarm".into(),
            },
        ),
    ]);
    timeline.start(0);
    let events = timeline.tick(1_000);
    assert_eq!(events.len(), 2);
    let warnings = run_all(&mut director, &pb, &events);
    assert_eq!(warnings.len(), 1);
    assert_eq!(director.active_scene(), Some("alarm"));
}

#[test]
fn stop_all_uses_the_requested_fade() {
    let (mut director, pb) = rig();
    director.run_timer_action(
        &pb,
        &TimerAction::ActivateScene {
            scene: "alarm".into(),
        },
    );
    pb.clear_calls();
    let run = director.run_timer_action(&pb, &TimerAction::StopAll { fade_ms: 1500 });
    assert!(run.is_clean());
    assert!(pb
        .calls()
        .iter()
        .any(|call| matches!(call, MockCall::StopAll { fade_ms: 1500 })));
    assert_eq!(director.active_scene(), None);
}

#[test]
fn timers_roundtrip_through_toml() {
    let config = Config {
        timers: vec![
            timer(
                "a",
                TimerTrigger::AfterMs(600_000),
                TimerAction::ActivateScene {
                    scene: "scene-id".into(),
                },
            ),
            timer(
                "b",
                TimerTrigger::EveryMs(1_800_000),
                TimerAction::PlayStinger {
                    sound: "sound-id".into(),
                },
            ),
            TimerConfig {
                enabled: false,
                ..timer(
                    "c",
                    TimerTrigger::AfterMs(5_000),
                    TimerAction::StopAll { fade_ms: 3_000 },
                )
            },
            timer("d", TimerTrigger::AfterMs(5_000), TimerAction::NextTrack),
        ],
        ..Config::default()
    };
    let text = toml::to_string_pretty(&config).unwrap();
    let back: Config = toml::from_str(&text).unwrap();
    assert_eq!(back.timers, config.timers, "{text}");
}

#[test]
fn hand_written_timers_table_parses() {
    let text = r#"
        [[timers]]
        id = "x"
        name = "Alarm"
        trigger = { after_ms = 600000 }
        action = { kind = "activate_scene", scene = "abc" }

        [[timers]]
        id = "y"
        trigger = { every_ms = 1800000 }
        action = { kind = "stop_all" }
        enabled = false
    "#;
    let config: Config = toml::from_str(text).unwrap();
    assert_eq!(config.timers.len(), 2);
    assert_eq!(config.timers[0].trigger, TimerTrigger::AfterMs(600_000));
    assert!(config.timers[0].enabled, "enabled defaults to true");
    assert_eq!(config.timers[1].action, TimerAction::StopAll { fade_ms: 0 });
    assert!(!config.timers[1].enabled);
}

#[test]
fn configs_without_timers_still_load_and_stay_clean() {
    let config: Config = toml::from_str("[server]\nport = 9000\n").unwrap();
    assert!(config.timers.is_empty());
    let text = toml::to_string_pretty(&config).unwrap();
    assert!(!text.contains("timers"), "{text}");
}

#[test]
fn runner_fires_pauses_and_resets_on_its_own_thread() {
    let runner = TimelineRunner::start(
        vec![timer(
            "t",
            TimerTrigger::AfterMs(200),
            TimerAction::StopScene,
        )],
        None,
    );
    assert_eq!(runner.snapshot().status, TimelineStatus::Stopped);
    runner.start_timers();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut event = None;
    while event.is_none() && Instant::now() < deadline {
        event = runner.try_event();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(event.map(|e| e.id), Some("t".to_string()));

    runner.pause();
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(runner.snapshot().status, TimelineStatus::Paused);
    runner.reset();
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(runner.snapshot().status, TimelineStatus::Stopped);
    assert!(runner.try_event().is_none());
}

//! Output / preview device selection.
//!
//! Nothing here depends on a particular sound card: the selection logic works
//! on plain device lists, the mixer is driven by hand and the engine tests
//! accept both "a device opened" and "no device here" as valid outcomes. The
//! config tests use temporary directories, never the user's config.toml.
//!
//! Windows (WASAPI) and Linux (ALSA/JACK) device naming and enumeration are not
//! covered: this suite was run on macOS only.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use soundshared::audio::engine::{LocalOutputState, LocalOutputStatus};
use soundshared::audio::{
    pick_output_device, plan_preview_route, AudioEngine, AudioMixer, Bus, DecodedAudio, DevicePick,
    DuckSettings, OutputDeviceInfo, OutputSelection, PreviewPlan,
};
use soundshared::config::{AudioConfig, Config};

// ---------------------------------------------------------------------------
// Allocation counting (per thread), for the preview callback
// ---------------------------------------------------------------------------

struct CountingAlloc;

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

fn note() {
    let _ = ARMED.try_with(|armed| {
        if armed.get() {
            let _ = COUNT.try_with(|count| count.set(count.get() + 1));
        }
    });
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note();
        System.alloc_zeroed(layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

fn allocations_in(f: impl FnOnce()) -> usize {
    COUNT.with(|c| c.set(0));
    ARMED.with(|a| a.set(true));
    f();
    ARMED.with(|a| a.set(false));
    COUNT.with(|c| c.get())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn devices() -> Vec<OutputDeviceInfo> {
    vec![
        OutputDeviceInfo::named("Built-in Output", true),
        OutputDeviceInfo::named("USB Headphones", false),
        OutputDeviceInfo::named("HDMI", false),
    ]
}

fn constant(mixer: &AudioMixer, value: f32, frames: usize, looped: bool) -> uuid::Uuid {
    let audio = Arc::new(DecodedAudio::new(vec![value; frames * 2], 48_000, 2));
    mixer.add_track(audio, looped)
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bardwave-audio-devices-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// Device choice and fallback
// ---------------------------------------------------------------------------

#[test]
fn no_name_means_the_system_default() {
    assert_eq!(pick_output_device(&devices(), None), DevicePick::Default);
}

#[test]
fn a_named_device_is_found_and_a_missing_one_falls_back() {
    assert_eq!(
        pick_output_device(&devices(), Some("HDMI")),
        DevicePick::Named(2)
    );
    let pick = pick_output_device(&devices(), Some("Bluetooth Speaker"));
    assert!(matches!(pick, DevicePick::Missing(_)));
    assert!(pick
        .fallback_message()
        .unwrap()
        .contains("Bluetooth Speaker"));
}

#[test]
fn the_preview_route_never_opens_the_main_device_twice() {
    let list = devices();
    assert_eq!(
        plan_preview_route(&list, "Built-in Output", None),
        PreviewPlan::SharedWithMain
    );
    assert_eq!(
        plan_preview_route(&list, "Built-in Output", Some("Built-in Output")),
        PreviewPlan::SharedWithMain
    );
    assert_eq!(
        plan_preview_route(&list, "Built-in Output", Some("USB Headphones")),
        PreviewPlan::Separate(1)
    );
    assert!(matches!(
        plan_preview_route(&list, "Built-in Output", Some("Unplugged")),
        PreviewPlan::Unavailable(_)
    ));
}

// ---------------------------------------------------------------------------
// The preview bus
// ---------------------------------------------------------------------------

#[test]
fn a_preview_is_not_ducked_and_ignores_the_bus_fader() {
    let mixer = AudioMixer::new();
    let background = constant(&mixer, 0.5, 48_000, true);
    let preview = constant(&mixer, 0.25, 48_000, true);
    mixer.set_track_local_only(preview, true).unwrap();
    mixer.play_track(background).unwrap();
    mixer.play_track(preview).unwrap();

    mixer.set_bus_volume(Bus::Ambience, 0.1).unwrap();
    mixer.duck_bus(
        Bus::Ambience,
        DuckSettings {
            target: 0.0,
            attack_ms: 1,
            release_ms: 1,
            hold_ms: None,
        },
    );

    let mut out = vec![0.0f32; 960];
    for _ in 0..30 {
        out.fill(0.0);
        mixer.mix_into(&mut out);
        mixer.mix_local_into(&mut out);
    }
    assert!(
        out.iter().all(|s| (*s - 0.25).abs() < 1e-3),
        "only the preview is left, at its own level: {:?}",
        &out[..4]
    );
}

#[test]
fn the_local_limiter_comes_back_gradually() {
    let mixer = AudioMixer::new();
    let loud = constant(&mixer, 0.9, 48_000 * 5, true);
    let preview = constant(&mixer, 0.9, 2_400, false);
    mixer.set_track_local_only(preview, true).unwrap();
    mixer.play_track(loud).unwrap();
    mixer.play_track(preview).unwrap();

    let block = || {
        let mut out = vec![0.0f32; 960];
        mixer.mix_into(&mut out);
        mixer.mix_local_into(&mut out)
    };
    while block() {}
    let reduced = mixer.local_limiter_gain();
    assert!(reduced < 1.0);
    block();
    let after = mixer.local_limiter_gain();
    assert!(after > reduced && after < 1.0, "{reduced} -> {after}");
}

#[test]
fn the_second_device_callback_does_not_allocate_and_never_publishes() {
    let mixer = AudioMixer::new();
    let preview = constant(&mixer, 0.3, 48_000, true);
    mixer.set_track_local_only(preview, true).unwrap();
    mixer.play_track(preview).unwrap();
    mixer.set_preview_external(true);
    let rx = mixer.broadcast_output_receiver();

    let mut out = vec![0.0f32; 960];
    mixer.mix_preview_into(&mut out); // warm up
    let allocations = allocations_in(|| {
        for _ in 0..50 {
            mixer.mix_preview_into(&mut out);
        }
    });
    assert_eq!(allocations, 0, "the preview callback must not allocate");
    assert!(out.iter().all(|s| (*s - 0.3).abs() < 1e-6));
    assert!(rx.try_recv().is_err(), "players never hear previews");
}

// ---------------------------------------------------------------------------
// Engine: applying a selection on a running engine
// ---------------------------------------------------------------------------

#[test]
fn the_selection_is_only_stored_while_the_engine_is_stopped() {
    let engine = AudioEngine::new().unwrap();
    let selection = OutputSelection {
        output: Some("USB Headphones".into()),
        preview: Some("HDMI".into()),
    };
    let status = engine.apply_output_selection(selection.clone()).unwrap();
    assert_eq!(status.state, LocalOutputState::Stopped);
    assert_eq!(engine.output_selection(), selection);
    assert_eq!(engine.preview_output_status(), LocalOutputStatus::stopped());
}

#[test]
fn a_vanished_output_device_falls_back_and_keeps_the_tracks_playing() {
    let engine = AudioEngine::new().unwrap();
    engine.start_headless_output().unwrap();
    let sound =
        engine.load_sound_from_data(DecodedAudio::new(vec![0.0; 48_000 * 2 * 20], 48_000, 2));
    let track = engine.play(sound).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(60));
    let before = track.position_ms().unwrap();

    let status = engine
        .apply_output_selection(OutputSelection {
            output: Some("No Such Device (bardwave test)".into()),
            preview: None,
        })
        .unwrap();

    match status.state {
        // A sound card exists here: the default was opened instead.
        LocalOutputState::Running => {
            assert!(
                status.reason.as_deref().unwrap_or("").contains("not found"),
                "the fallback is explained: {status:?}"
            );
        }
        // No sound card (CI): the engine carries on headless and says why.
        LocalOutputState::Headless => assert!(status.reason.is_some()),
        other => panic!("unexpected state {other:?}"),
    }
    assert!(engine.is_running());
    assert!(track.is_playing().unwrap(), "the track survived the switch");
    assert!(
        track.position_ms().unwrap() >= before,
        "and did not restart"
    );
    engine.stop_output().unwrap();
}

#[test]
fn a_missing_preview_device_keeps_previews_on_the_main_output() {
    let engine = AudioEngine::new().unwrap();
    engine.start_headless_output().unwrap();
    let sound =
        engine.load_sound_from_data(DecodedAudio::new(vec![0.0; 48_000 * 2 * 5], 48_000, 2));

    engine
        .apply_output_selection(OutputSelection {
            output: None,
            preview: Some("No Such Headphones (bardwave test)".into()),
        })
        .unwrap();

    let preview = engine.preview_output_status();
    match &preview.state {
        LocalOutputState::Failed(message) => {
            assert!(message.contains("not found"), "{message}");
            assert!(message.contains("main output"), "{message}");
        }
        other => panic!("expected a failed preview route, got {other:?}"),
    }
    let handle = engine
        .play_preview(sound, false)
        .expect("preview still plays");
    assert!(handle.is_playing().unwrap());
    engine.stop_output().unwrap();
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[test]
fn device_settings_round_trip_through_config_toml() {
    let dir = temp_dir("roundtrip");
    let path = dir.join("config.toml");

    let mut config = Config::default();
    config.audio.output_device = Some("USB DAC (Rev. 2)".into());
    config.audio.preview_device = Some("Headphones \"Studio\"".into());
    config.save_to(&path).unwrap();

    let loaded = Config::load_from(&path).unwrap();
    assert_eq!(
        loaded.audio.output_device.as_deref(),
        Some("USB DAC (Rev. 2)")
    );
    assert_eq!(
        loaded.audio.preview_device.as_deref(),
        Some("Headphones \"Studio\"")
    );
    assert_eq!(
        OutputSelection::from_audio_config(&loaded.audio),
        OutputSelection {
            output: Some("USB DAC (Rev. 2)".into()),
            preview: Some("Headphones \"Studio\"".into()),
        }
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unset_devices_stay_out_of_the_file_and_old_configs_still_load() {
    let dir = temp_dir("compat");
    let path = dir.join("config.toml");

    // A config written before these fields existed.
    std::fs::write(
        &path,
        "[audio]\nbuffer_size = 480\nsample_rate = 48000\nchannels = 2\nmaster_volume = 0.7\n",
    )
    .unwrap();
    let loaded = Config::load_from(&path).unwrap();
    assert_eq!(loaded.audio.output_device, None);
    assert_eq!(loaded.audio.preview_device, None);
    assert_eq!(loaded.audio.master_volume, 0.7);

    // Saving it back adds no device keys (the default stays "system default").
    loaded.save_to(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("output_device"), "{text}");
    assert!(!text.contains("preview_device"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_default_audio_config_selects_nothing() {
    let audio = AudioConfig::default();
    assert_eq!(
        OutputSelection::from_audio_config(&audio),
        OutputSelection::default()
    );
}

//! The mix callback must not allocate: fades, pan, buses, ducking and local
//! previews all run inside it.
//!
//! A counting global allocator (per thread, so parallel tests cannot disturb
//! each other) watches the mix calls. This test binary has its own allocator;
//! it does not affect the rest of the suite.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use soundshared::audio::{AudioMixer, Bus, DecodedAudio, DuckSettings, FadeCurve};

struct CountingAlloc;

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

fn note() {
    // `try_with`: the thread-local may already be gone during thread teardown.
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

/// Number of allocations `f` makes on this thread.
fn allocations_in(f: impl FnOnce()) -> usize {
    COUNT.with(|c| c.set(0));
    ARMED.with(|a| a.set(true));
    f();
    ARMED.with(|a| a.set(false));
    COUNT.with(|c| c.get())
}

fn track(mixer: &AudioMixer, value: f32, frames: usize, looped: bool) -> uuid::Uuid {
    mixer.add_track(
        Arc::new(DecodedAudio::new(vec![value; frames * 2], 48_000, 2)),
        looped,
    )
}

/// The counter itself works (otherwise the zero below would prove nothing).
#[test]
fn counting_allocator_sees_allocations() {
    let n = allocations_in(|| {
        let v = vec![1u8; 100];
        std::hint::black_box(&v);
    });
    assert!(n >= 1);
}

#[test]
fn mix_callback_does_not_allocate() {
    let mixer = AudioMixer::new();
    let music = track(&mixer, 0.2, 48_000, true);
    let amb = track(&mixer, 0.2, 777, true); // short: wraps inside every block
    let sfx = track(&mixer, 0.2, 100_000, false);
    let preview = track(&mixer, 0.3, 48_000, true);
    mixer.set_track_bus(music, Bus::Music).unwrap();
    mixer.set_track_bus(sfx, Bus::Sfx).unwrap();
    mixer.set_track_local_only(preview, true).unwrap();
    mixer.set_track_pan(amb, -0.6).unwrap();
    mixer.play_track(music).unwrap();
    mixer
        .play_track_with_fade_in(amb, 500, FadeCurve::EqualPower)
        .unwrap();
    mixer.play_track(sfx).unwrap();
    mixer.play_track(preview).unwrap();
    mixer.set_master_volume(0.8).unwrap();

    // Sizes beyond the internal chunk and odd ones, like real device buffers.
    let mut buffers: Vec<Vec<f32>> = [480usize, 1, 37, 512, 513, 1024, 4096, 480]
        .iter()
        .map(|frames| vec![0.0f32; frames * 2])
        .collect();

    let mut round = 0;
    let n = allocations_in(|| {
        for _ in 0..40 {
            for buf in buffers.iter_mut() {
                // Control changes happen on the GUI thread; the mix calls
                // after them are what must stay allocation-free.
                mixer.mix_into(buf);
                mixer.mix_local_into(buf);
                round += 1;
            }
        }
    });
    assert_eq!(n, 0, "mix_into / mix_local_into allocated {n} times");
    assert!(round > 0);

    // Same with fades ending, a duck in motion and a stop in flight. The
    // control calls are made outside the measured section.
    mixer
        .fade_track_to(music, 0.0, 200, FadeCurve::Linear)
        .unwrap();
    mixer
        .stop_track_with_fade_out(amb, 100, FadeCurve::EqualPower)
        .unwrap();
    mixer.duck_bus(Bus::Sfx, DuckSettings::default());
    mixer.set_bus_volume(Bus::Music, 0.5).unwrap();
    let n = allocations_in(|| {
        for _ in 0..200 {
            for buf in buffers.iter_mut() {
                mixer.mix_into(buf);
                mixer.mix_local_into(buf);
            }
        }
    });
    assert_eq!(n, 0, "transitions allocated {n} times");
}

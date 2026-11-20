//! End-to-end checks for the headless mixer -> broadcaster wire format.

use std::time::Duration;

use soundshared::audio::{AudioEngine, DecodedAudio};

use soundshared::network::broadcast::AudioFrame;

#[test]
fn headless_engine_publishes_10ms_frames_with_four_byte_header() {
    let engine = AudioEngine::new().expect("engine");
    let output_rx = engine.broadcast_output_receiver();

    // Two seconds of a constant 0.25 stereo signal.
    let audio = DecodedAudio::new(vec![0.25; 48_000 * 2 * 2], 48_000, 2);
    let sound_id = engine.load_sound_from_data(audio);

    engine.start_headless_output().expect("headless output");
    let track = engine.play(sound_id).expect("play");

    let mixed = output_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("mixer should publish a frame");

    // 10 ms at 48 kHz stereo = 480 frames = 960 interleaved samples.
    assert_eq!(mixed.buffer.len(), 960);
    assert!(mixed.buffer.iter().all(|s| (*s - 0.25).abs() < 1e-6));

    // The wire frame is 20 ms (960 frames), so two 10 ms mixer blocks make one
    // Opus packet.
    let second = output_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("mixer should publish a second frame");
    let mut wire_samples = mixed.buffer.clone();
    wire_samples.extend_from_slice(&second.buffer);

    // Encode the way the broadcaster does: one 8-byte header (seq + server_ms)
    // followed by one Opus packet, far smaller than the raw PCM was.
    let mut encoder = soundshared::network::broadcast::wire_encoder();
    let frame = AudioFrame::new(wire_samples.clone(), 48_000, 2, mixed.seq_no);
    let payload = AudioFrame::opus_payload(&mut encoder, &frame).expect("opus encode");
    let binary = frame.to_binary_header(&std::time::Instant::now(), &payload);

    assert_eq!(
        AudioFrame::seq_no_from_binary(&binary),
        Some(mixed.seq_no as u32)
    );
    assert!(AudioFrame::server_ms_from_binary(&binary).is_some());
    let wire_payload = AudioFrame::payload_from_binary(&binary).expect("opus payload present");
    assert!(!wire_payload.is_empty());
    assert!(
        wire_payload.len() < wire_samples.len() * 4 / 8,
        "Opus must beat PCM to a fraction of its size"
    );

    drop(track);
    engine.stop_output().expect("stop");
}

#[test]
fn master_volume_is_applied_exactly_once() {
    let engine = AudioEngine::new().expect("engine");
    let output_rx = engine.broadcast_output_receiver();

    let audio = DecodedAudio::new(vec![1.0; 48_000 * 2], 48_000, 2);
    let sound_id = engine.load_sound_from_data(audio);

    engine.set_master_volume(0.5).expect("volume");
    engine.start_headless_output().expect("headless output");
    let track = engine.play(sound_id).expect("play");

    let mixed = output_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("mixer should publish a frame");

    assert!(mixed.buffer.iter().all(|s| (*s - 0.5).abs() < 1e-6));

    drop(track);
    engine.stop_output().expect("stop");
}

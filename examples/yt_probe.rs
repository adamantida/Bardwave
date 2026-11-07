//! Manual integration probe for the YouTube pipeline (run by hand):
//!
//! ```bash
//! cargo run --example yt_probe -- <youtube-url>
//! ```
//!
//! Runs the exact pipeline the console uses — piped yt-dlp output, progress
//! relay, ffmpeg postprocessing, Symphonia decode — and prints the result.
//! Not part of `cargo test` — it uses the network.

use soundshared::audio::youtube::{self, StatusSink};
use std::sync::Arc;

fn main() {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://www.youtube.com/watch?v=jNQXAC9IVRw".to_string());
    let Some(id) = youtube::extract_video_id(&url) else {
        eprintln!("not a YouTube video link: {url}");
        std::process::exit(64);
    };
    println!("resolved video id: {id}");

    let start = std::time::Instant::now();
    let sink: StatusSink = Arc::new(|line| println!("[status] {line}"));
    match youtube::download_audio(&url, &id, &sink) {
        Ok(path) => {
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            println!(
                "OK: {} ({size} bytes) in {:.1}s",
                path.display(),
                start.elapsed().as_secs_f32()
            );
            // The exact path the app takes next: Symphonia decode to 48k stereo.
            match soundshared::audio::AudioDecoder::decode_file(&path) {
                Ok(audio) => println!(
                    "DECODE OK: {} frames, {:.2}s, {} Hz / {} ch",
                    audio.frames, audio.duration, audio.sample_rate, audio.channels
                ),
                Err(e) => {
                    eprintln!("DECODE FAILED: {e}");
                    std::process::exit(2);
                }
            }
            println!("display name: {}", youtube::display_name(&path));
        }
        Err(e) => {
            eprintln!("FAILED: {e}");
            std::process::exit(1);
        }
    }
}

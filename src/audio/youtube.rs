//! YouTube links as a sound source.
//!
//! The operator pastes a YouTube video / music link into the console; the audio
//! stream is downloaded once with [`yt-dlp`](https://github.com/yt-dlp/yt-dlp),
//! stored in `bardwave-library/` next to the executable (so the saved library
//! keeps working after a restart) and then played back exactly like any local
//! file — the existing Symphonia decoder handles the result.
//!
//! The heavy lifting stays in an external tool on purpose: YouTube's stream
//! extraction changes every few weeks while `yt-dlp` is updated weekly, so
//! re-implementing its logic would break constantly. This module only
//! identifies videos (pure, unit-tested logic) and drives the tool.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Every supported URL form carries an 11-character video id.
const ID_LEN: usize = 11;

/// Cap on the title part of a downloaded file name, in characters. Leaves the
/// ` [id]` stamp comfortably inside Windows' name limit.
const MAX_TITLE_LEN: usize = 80;

/// Windows device names that cannot appear as a file name, in any namespace:
/// even `con.mp3` or `NUL title.mp3` refuse to be created.
const DEVICE_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Extract the 11-character video id from a YouTube URL (or from a bare id).
///
/// Supported shapes:
/// - `https://www.youtube.com/watch?v=<id>&t=…`
/// - `https://youtu.be/<id>?si=…`
/// - `https://music.youtube.com/watch?v=<id>`
/// - `https://www.youtube.com/shorts/<id>`, `/embed/<id>`, `/live/<id>`
/// - a bare `<id>`
///
/// Anything else (playlists, channel pages, other sites) is rejected — the
/// caller reports "Not a YouTube link" instead of downloading a surprise.
pub fn extract_video_id(input: &str) -> Option<String> {
    // Whitespace (a wrapped link, a stray trailing space) never belongs to one.
    let text: String = input.split_whitespace().collect::<String>();
    if text.is_empty() {
        return None;
    }

    // A bare id ("dQw4w9WgXcQ") is accepted for convenience.
    if !text.contains("://") && !text.contains('.') {
        return id_from_start(&text).map(str::to_string);
    }

    // The scheme is optional ("youtu.be/x" pasted from an address bar).
    let after_scheme = text
        .split_once("://")
        .map_or(text.as_str(), |(_scheme, rest)| rest);
    let host = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();

    // Only YouTube hosts so `evil-site.com/watch?v=…` gets the extra id without
    // being served as a Bardwave sound.
    if text.contains("youtu.be/") || is_youtube_host(&host) {
        let pick = |needle: &str| -> Option<String> {
            let pos = after_scheme.find(needle)?;
            id_from_start(&after_scheme[pos + needle.len()..]).map(str::to_string)
        };
        pick("youtu.be/")
            .or_else(|| pick("watch?v="))
            .or_else(|| pick("/shorts/"))
            .or_else(|| pick("/embed/"))
            .or_else(|| pick("/live/"))
    } else {
        None
    }
}

/// True for `youtube.com` with any subdomain (`www.`, `m.`, `music.`) and the
/// `youtube-nocookie.com` embed variant, but not for lookalike hosts.
fn is_youtube_host(host: &str) -> bool {
    host == "youtube.com"
        || host == "youtu.be"
        || host.ends_with(".youtube.com")
        || host.ends_with(".youtube-nocookie.com")
}

/// Take an id from the beginning of `text`, stopping at the first character
/// that cannot occur in an id (`&`, `?`, `/` — anything query-ish).  The
/// character class also acts as a validity check.
fn id_from_start(text: &str) -> Option<&str> {
    let end = text
        .char_indices()
        .take(ID_LEN + 1)
        .find(|(_, c)| !c.is_ascii_alphanumeric() && *c != '_' && *c != '-')
        .map_or(text.len(), |(i, _)| i);
    let id = text.get(..end)?;
    if id.len() == ID_LEN
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        Some(id)
    } else {
        None
    }
}

/// Where download progress lines go (the GUI updates a status label).
pub type StatusSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Locate an external tool: next to the executable, then in the user's
/// app-data tools folder, then on `PATH`.
///
/// Bardwave is a portable single binary, so shipping the tool next to it works;
/// installing it system-wide (winget adds it to `PATH`) works too — and when
/// neither holds, [`ensure_yt_dlp`] fetches it into one of the first two.
fn find_tool(name: &str) -> Option<PathBuf> {
    let names = if cfg!(windows) {
        vec![format!("{name}.exe"), name.to_string()]
    } else {
        vec![name.to_string()]
    };

    for dir in tool_dirs() {
        for candidate in &names {
            let path = dir.join(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
    }

    // Somewhere on PATH.
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        for candidate in &names {
            let path = dir.join(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

/// Directories the external tools may live in, in priority order.
fn tool_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    // Next to the executable — the same place config.toml lives. In a dev
    // checkout that is the project's `target/<profile>/` folder; in a portable
    // layout it is whatever folder the operator runs the binary from.
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        dirs.push(dir);
    }
    // The user's app-data tools folder: the fallback when the executable
    // directory is not writable (installed into Program Files, say).
    if let Ok(dir) = user_tools_dir() {
        dirs.push(dir);
    }
    dirs
}

/// Per-user tools folder (`%LOCALAPPDATA%\Bardwave\tools` on Windows,
/// `~/.local/share/Bardwave/tools` on Linux/macOS, or `$XDG_DATA_HOME`).
fn user_tools_dir() -> Result<PathBuf, String> {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").ok_or_else(|| "LOCALAPPDATA is not set".to_string())?
    } else if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        xdg
    } else {
        let home = std::env::var_os("HOME").ok_or_else(|| "HOME is not set".to_string())?;
        PathBuf::from(home).join(".local/share").into_os_string()
    };
    Ok(PathBuf::from(base).join("Bardwave").join("tools"))
}

/// Whether the `yt_dlp` bundled command is present, downloading it once if not.
///
/// The fetch goes straight from yt-dlp's GitHub `latest` release (a redirect to
/// the current version, so the tool stays fresh without our involvement) into
/// the first writable tools directory. Progress lines go to `status` for the
/// dialog. On success the download is cached, so this is a one-time ~24 MB.
pub fn ensure_yt_dlp(status: &StatusSink) -> Result<PathBuf, String> {
    if let Some(path) = find_tool("yt-dlp") {
        return Ok(path);
    }

    let Some((asset, name)) = yt_dlp_asset() else {
        return Err(
            "This platform has no prebuilt yt-dlp — please install it manually \
             (pip install yt-dlp) and put it on the PATH: https://github.com/yt-dlp/yt-dlp"
                .to_string(),
        );
    };
    status("yt-dlp not found — downloading it once (~24 MB)…");
    let dir = writable_tool_dir(name)?;
    let target = dir.join(name);

    download_file(
        &format!("https://github.com/yt-dlp/yt-dlp/releases/latest/download/{asset}"),
        &target,
        "yt-dlp",
        1_000_000,
        status,
    )?;
    make_executable(&target);

    // Re-locate strictly: proves the directory and file name agree.
    let tool = find_tool("yt-dlp").ok_or_else(|| {
        format!(
            "yt-dlp was downloaded to {} but could not be found again afterwards",
            target.display()
        )
    })?;
    status("yt-dlp ready");
    Ok(tool)
}

/// ffmpeg for the `--extract-audio` postprocessing: `true` when it is available
/// (already installed, or downloaded once from BtbN's stable builds — the same
/// source yt-dlp's wiki recommends — and unpacked next to yt-dlp); `false` on
/// non-Windows systems without it, where the caller refuses with install
/// instructions because YouTube's DASH audio cannot be decoded without it.
pub fn ensure_ffmpeg(status: &StatusSink) -> Result<bool, String> {
    if find_tool("ffmpeg").is_some() {
        return Ok(true);
    }
    if !cfg!(target_os = "windows") {
        return Ok(false);
    }

    status("ffmpeg not found — downloading it once (~100 MB)…");
    let dir = writable_tool_dir("ffmpeg.exe")?;
    let zip = dir.join("ffmpeg.zip");
    let url = "https://github.com/BtbN/FFmpeg-Builds/releases/latest/download/ffmpeg-master-latest-win64-gpl.zip";
    download_file(url, &zip, "ffmpeg", 50_000_000, status)?;
    extract_ffmpeg(&dir, &zip, status)?;
    let _ = std::fs::remove_file(&zip);

    if find_tool("ffmpeg").is_some() {
        Ok(true)
    } else {
        Err("ffmpeg.zip was unpacked, but ffmpeg.exe was not found in it".to_string())
    }
}

/// Unpack `ffmpeg.exe` out of the BtbN zip with Windows' own expansion tool —
/// the folder layout inside the archive is versioned, so the single file is
/// located by name rather than by path.
fn extract_ffmpeg(dir: &Path, zip: &Path, status: &StatusSink) -> Result<(), String> {
    let staging = dir.join("ffmpeg-unpack");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|e| format!("Could not create {}: {e}", staging.display()))?;

    status("Unpacking ffmpeg…");
    let mut command = std::process::Command::new("powershell");
    command.arg("-NoProfile").arg("-Command").arg(format!(
        "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
        zip.display(),
        staging.display()
    ));
    no_window(&mut command);
    let output = command
        .output()
        .map_err(|e| format!("Failed to run Expand-Archive: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "ffmpeg unpack failed: {}",
            last_lines(stderr.lines(), 400)
        ));
    }

    // `…/bin/ffmpeg.exe` somewhere under the versioned root.
    let found = std::fs::read_dir(&staging)
        .map_err(|e| {
            let _ = std::fs::remove_dir_all(&staging);
            format!("Could not list {}: {e}", staging.display())
        })?
        .filter_map(|entry| entry.ok())
        .find_map(|root| {
            std::fs::read_dir(root.path().join("bin"))
                .ok()?
                .filter_map(|entry| entry.ok())
                .find(|entry| entry.file_name() == "ffmpeg.exe")
                .map(|entry| entry.path())
        });
    let Some(exe) = found else {
        let _ = std::fs::remove_dir_all(&staging);
        return Err("ffmpeg.zip does not contain bin/ffmpeg.exe".to_string());
    };

    let target = dir.join("ffmpeg.exe");
    std::fs::rename(&exe, &target).map_err(|e| {
        let _ = std::fs::remove_dir_all(&staging);
        format!(
            "Failed to move {} to {}: {e}",
            exe.display(),
            target.display()
        )
    })?;
    let _ = std::fs::remove_dir_all(&staging);
    status("ffmpeg ready");
    Ok(())
}

/// Which GitHub release asset carries yt-dlp for the target OS, and the plain
/// executable name it is stored as (`yt-dlp` / `yt-dlp.exe`).
fn yt_dlp_asset() -> Option<(&'static str, &'static str)> {
    // Keep these in sync with yt-dlp's release-file names.
    if cfg!(target_os = "windows") {
        Some(("yt-dlp.exe", "yt-dlp.exe"))
    } else if cfg!(target_os = "macos") {
        Some(("yt-dlp_macos", "yt-dlp"))
    } else if cfg!(target_os = "linux") {
        if cfg!(target_arch = "aarch64") {
            Some(("yt-dlp_linux_aarch64", "yt-dlp"))
        } else {
            Some(("yt-dlp_linux", "yt-dlp"))
        }
    } else {
        None
    }
}

/// The first tools directory that can actually receive `name`.
///
/// Writability is probed for real: installed copies inside `Program Files`
/// accept `create_dir_all` (the folder already exists) but silently drop any
/// new file without elevation.
fn writable_tool_dir(name: &str) -> Result<PathBuf, String> {
    for dir in tool_dirs() {
        let probe = dir.join(name);
        if std::fs::create_dir_all(&dir).is_ok() {
            match std::fs::File::create(&probe) {
                Ok(_) => {
                    let _ = std::fs::remove_file(&probe);
                    return Ok(dir);
                }
                // A leftover file from a previous run is not a permission error.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(dir),
                Err(_) => {}
            }
        }
    }
    Err(
        "Neither the executable folder nor the app-data folder is writable for \
             the yt-dlp download"
            .to_string(),
    )
}

/// Stream `url` into `target`, keeping `status` fed with progress at most every
/// couple of megabytes. Written via a `.part` temp file so a half-download is
/// never mistaken for the real tool; `min_bytes` catches error pages and
/// truncated transfers instead of trusting them.
fn download_file(
    url: &str,
    target: &Path,
    label: &str,
    min_bytes: u64,
    status: &StatusSink,
) -> Result<(), String> {
    let part = target.with_extension(".part");
    let response = ureq::get(url)
        .call()
        .map_err(|e| format!("Failed to download {label}: {e}"))?;
    let total = response
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok());

    let mut reader = response.into_reader();
    let mut file = std::fs::File::create(&part)
        .map_err(|e| format!("Failed to create {}: {e}", part.display()))?;

    let mut buffer = vec![0u8; 64 * 1024];
    let mut received = 0u64;
    let mut next_report_mb = 2u64;
    loop {
        use std::io::Read;
        let n = reader
            .read(&mut buffer)
            .map_err(|e| format!("Download of {label} interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        use std::io::Write;
        file.write_all(&buffer[..n])
            .map_err(|e| format!("Failed to write {}: {e}", part.display()))?;
        received += n as u64;

        let got_mb = received / 1_048_576;
        if got_mb >= next_report_mb {
            next_report_mb = got_mb + 2;
            match total {
                Some(total) => status(&format!(
                    "Downloading {label}… {got_mb}/{} MB",
                    total / 1_048_576
                )),
                None => status(&format!("Downloading {label}… {got_mb} MB")),
            }
        }
    }

    // A truncated transfer or an error page below `min_bytes` would otherwise
    // produce a baffling failure far later in the pipeline.
    if received < min_bytes {
        let _ = std::fs::remove_file(&part);
        return Err(format!(
            "Downloaded {label} is only {received} bytes — aborted (release server error?)"
        ));
    }

    std::fs::rename(&part, target).map_err(|e| {
        format!(
            "Failed to move {} to {}: {e}",
            part.display(),
            target.display()
        )
    })?;
    Ok(())
}

/// Executable permission for the downloaded tool on Unix.
fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(path) {
            let _ = std::fs::set_permissions(
                path,
                std::fs::Permissions::from_mode(metadata.permissions().mode() | 0o755),
            );
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Where downloaded audio is stored.
///
/// The folder sits next to `config.toml` (next to the executable), so a library
/// entry keeps pointing at an existing file after a restart, exactly like local
/// sounds the operator added from their own disk.
pub fn downloads_dir() -> Result<PathBuf, String> {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .ok_or_else(|| "Could not locate the executable directory".to_string())?
        .join("bardwave-library");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Could not create {}: {}", dir.display(), e))?;
    Ok(dir)
}

/// Windows: keep the spawned console tools from flashing a terminal window.
fn no_window(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// Keep at most `max` characters from the *end* of some tool output: that is
/// where yt-dlp explains a failure ("ERROR: …").
fn last_lines<'a>(mut iter: impl Iterator<Item = &'a str>, max: usize) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut total = 0;
    for line in iter.by_ref() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        kept.push(line);
        total += line.chars().count() + 1;
        if total > max {
            kept.clear();
            total = 0;
        }
    }
    kept.join("\n")
}

/// Ask a Python child to speak UTF-8 on the pipes.
///
/// Windows ships yt-dlp as a frozen executable that inherits the console code
/// page (cp1251 on Russian systems, for example); forcing UTF-8 here means any
/// later decoding is a no-op instead of a fight with mojibake.
fn force_utf8(command: &mut std::process::Command) {
    command
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8");
}

/// Metadata-only yt-dlp query: resolve the video and print just its title.
///
/// The title travels through a file, not stdout: a frozen yt-dlp on Windows
/// encodes piped output in the console code page (cp1251 on Russian systems —
/// verified live), while `--print-to-file` writes its own UTF-8 file whatever
/// the locale is. Media is untouched (`--skip-download`) and the round trip is
/// fast; most importantly this fails early with yt-dlp's own explanation
/// (removed video, region lock, a typo in the id — ids are case-sensitive)
/// before any download attempt.
fn fetch_title(ytdlp: &Path, dir: &Path, url: &str, video_id: &str) -> Result<String, String> {
    let sidecar = dir.join(format!(".{video_id}.title"));
    let mut command = std::process::Command::new(ytdlp);
    command
        .arg("--no-playlist")
        .arg("--no-warnings")
        .arg("--skip-download")
        .arg("--print-to-file")
        .arg("title")
        .arg(&sidecar)
        .arg(url);
    no_window(&mut command);
    force_utf8(&mut command);

    let output = command
        .output()
        .map_err(|e| format!("Failed to run yt-dlp: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "yt-dlp failed: {}",
            last_lines(stderr.lines(), 400)
        ));
    }
    let title = std::fs::read(&sidecar)
        .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_string())
        .map_err(|e| format!("yt-dlp did not write the title file: {e}"))?;
    let _ = std::fs::remove_file(&sidecar);
    if title.is_empty() {
        return Err(format!("yt-dlp returned no title for {video_id}"));
    }
    Ok(title)
}

/// Turn a video title into a safe Windows file-name stem.
///
/// The forbidden punctuation becomes `_`, control characters are dropped, runs
/// of blanks fold into one space, trailing dots/spaces go away (Windows strips
/// them anyway, breaking the ` [id]` match), the length is capped and device
/// names (`CON`, `COM1`…) get a prefix so the name is creatable. YouTube titles
/// are operator-chosen input, so this is done in Rust and never via the child's
/// `%(title)` substitution — its sanitizer is what produced Windows error 123
/// ("unable to open for writing") on a real title.
fn sanitize_windows_name(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut pending_space = false;
    for c in title.chars() {
        if c.is_control() {
            continue;
        }
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            },
        );
    }
    let mut stem: String = out
        .trim_matches(['.', ' '])
        .chars()
        .take(MAX_TITLE_LEN)
        .collect();
    if let Some(first) = stem.split(['.', ' ']).next() {
        if DEVICE_NAMES.contains(&first.to_ascii_uppercase().as_str()) {
            stem = format!("_{stem}");
        }
    }
    if stem.is_empty() {
        "video".to_string()
    } else {
        stem
    }
}

/// Double the `%` of a literal name to be used inside an yt-dlp output
/// template: there `%%` is the escape for one literal percent, so a title like
/// "100% Live" keeps its percent instead of eating the `%(ext)s` after it.
fn escape_output_template(name: &str) -> String {
    name.replace('%', "%%")
}

/// Download one video's audio into the managed library folder.
///
/// `status` receives human-readable progress lines for the dialog.  The file
/// name carries `<title> [<id>]` so:
/// - the row shows a real title, not a base64-looking file name;
/// - re-adding the same link is free — the existing file is found by the stamp
///   before any network happens.
///
/// Returns the path of the finished audio file.
pub fn download_audio(url: &str, video_id: &str, status: &StatusSink) -> Result<PathBuf, String> {
    // Fetches yt-dlp on first use (and verifies it afterwards, every time).
    status("Connecting…");
    let ytdlp = ensure_yt_dlp(status)?;

    let dir = downloads_dir()?;

    // A previous download of this video is reused as is — the ` [id]` stamp
    // covers both the current naming and what older builds left behind.
    if let Some(existing) = find_downloaded_file(&dir, video_id) {
        status("Already downloaded — reusing");
        return Ok(existing);
    }

    // ffmpeg completes the `--extract-audio` postprocessing: with it, any
    // YouTube stream is transcoded to MP3 and Symphonia decodes it everywhere.
    // Without it, the DASH audio streams (fragmented MP4 / Opus-in-WebM) stay
    // outside what Symphonia reads, so the download must fail with an
    // actionable message instead of producing a file the decoder rejects
    // at play time.
    if !ensure_ffmpeg(status)? {
        return Err(
            "ffmpeg not found on this system. YouTube only serves DASH audio streams \
             that need ffmpeg to extract; install ffmpeg (apt install ffmpeg / \
             brew install ffmpeg) or place ffmpeg.exe next to Bardwave.exe and retry"
                .to_string(),
        );
    }

    // Phase 1: the cheap metadata query, with an early, self-explaining error.
    status("Fetching video info…");
    let title = fetch_title(&ytdlp, &dir, url, video_id)?;

    // Phase 2: download straight into a name built here — the id is always
    // valid, and the sanitized title is passed as a literal template value.
    let stem = format!("{} [{video_id}]", sanitize_windows_name(&title));
    let template = dir.join(format!("{}.%(ext)s", escape_output_template(&stem)));
    let mut command = std::process::Command::new(&ytdlp);
    command
        .arg("--no-playlist") // a link pasted with &list=… stays one track
        .arg("--windows-filenames") // the library is portable across machines
        .arg("--no-overwrites")
        .arg("--no-warnings") // keep stderr useful for error reporting
        .arg("-o")
        .arg(&template)
        .arg("-f")
        .arg("bestaudio[ext=m4a]/bestaudio")
        .arg("-x")
        .arg("--audio-format")
        .arg("mp3")
        .arg("--audio-quality")
        .arg("160K")
        .arg(url);
    no_window(&mut command);
    force_utf8(&mut command);

    // yt-dlp only prints its rich progress when it has a terminal; piped, the
    // modal would sit on "Downloading audio…" with no sign of life. With
    // `--newline` it emits one `[download] <pct>% …` line per update, which
    // `run_with_progress` relays into the dialog in real time.
    status("Downloading audio…");
    command.arg("--newline");
    let run = run_with_progress(&mut command, status)?;

    if !run.success {
        return Err(format!(
            "yt-dlp failed: {}",
            last_lines(run.stderr_text.lines(), 400)
        ));
    }

    find_downloaded_file(&dir, video_id).ok_or_else(|| {
        format!(
            "Download finished, but no file for {video_id} appeared in {}",
            dir.display()
        )
    })
}

/// Outcome of a piped yt-dlp run: exit code plus the stderr tail for errors.
struct YtdlpRun {
    success: bool,
    stderr_text: String,
}

/// Run `command` with fully piped output, relaying its progress lines into
/// `status` while it works.
///
/// yt-dlp writes progress to one stream and diagnostics to the other (and the
/// destination can change between minors), so both pipes are read and parsed
/// with the same rules. Each reader keeps the last lines so a failure can be
/// quoted to the operator.
fn run_with_progress(
    command: &mut std::process::Command,
    status: &StatusSink,
) -> Result<YtdlpRun, String> {
    use std::process::Stdio;

    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| format!("Failed to run yt-dlp: {e}"))?;

    let tail: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let report = status.clone();

    let mut readers = Vec::new();
    if let Some(pipe) = child.stdout.take() {
        let tail = tail.clone();
        let report = report.clone();
        readers.push(std::thread::spawn(move || {
            read_progress_lines(pipe, tail, &*report)
        }));
    }
    if let Some(pipe) = child.stderr.take() {
        let tail = tail.clone();
        let report = report.clone();
        readers.push(std::thread::spawn(move || {
            read_progress_lines(pipe, tail, &*report)
        }));
    }

    let result = child
        .wait()
        .map_err(|e| format!("yt-dlp run failed: {e}"))?;
    for reader in readers {
        let _ = reader.join();
    }
    let stderr_text = tail.lock().unwrap().join("\n");

    Ok(YtdlpRun {
        success: result.success(),
        stderr_text,
    })
}

/// Consume one output pipe line by line, updating the dialog on recognizable
/// progress marks and buffering everything for error reporting.
fn read_progress_lines<R: std::io::Read>(
    pipe: R,
    tail: Arc<Mutex<Vec<String>>>,
    report: &(dyn Fn(&str) + Send + Sync),
) {
    use std::io::{BufRead, BufReader};
    let mut reader = BufReader::new(pipe);
    let mut raw: Vec<u8> = Vec::new();
    // `read_until` + lossy decode instead of `lines()`: one invalid-UTF-8 byte
    // (the console code page can leak through despite the UTF-8 env) must not
    // kill the reader — a dead reader closes the pipe mid-run, and the child
    // then dies writing into it ("Errno 22 on flushing sys.stdout").
    loop {
        match reader.read_until(b'\n', &mut raw) {
            Ok(0) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&raw);
                let trimmed = line.trim_end();
                if let Ok(mut tail) = tail.lock() {
                    tail.push(trimmed.to_string());
                    if tail.len() > 40 {
                        let extra = tail.len() - 40;
                        tail.drain(..extra);
                    }
                }
                if let Some(message) = progress_message(trimmed) {
                    report(&message);
                }
                raw.clear();
            }
            Err(_) => break,
        }
    }
}

/// Map one yt-dlp output line to a dialog status line, or `None` to stay quiet.
fn progress_message(line: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix("[download] ") {
        if let Some(token) = rest.split_whitespace().next() {
            if token.ends_with('%') {
                return Some(format!("Downloading audio… {token}"));
            }
        }
    }
    // The ffmpeg postprocessing pass; its own ffmpeg stdout lines stay quiet.
    if line.starts_with("[ExtractAudio] ") || line.starts_with("[Merger] ") {
        return Some("Converting to MP3…".to_string());
    }
    None
}

/// The file carrying ` [video_id]` in its stem — the download is done.
fn find_downloaded_file(dir: &Path, video_id: &str) -> Option<PathBuf> {
    let stamp = format!(" [{video_id}]");
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .find(|entry| {
            entry
                .path()
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| stem.ends_with(&stamp))
        })
        .map(|entry| entry.path())
}

/// Row name for a sound: the file stem minus the downloader's ` [id]` stamp, so
/// a YouTube track lists as its title, like any other sound.
pub fn display_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Unknown");
    let Some(stamp_start) = stem.rfind(" [") else {
        return stem.to_string();
    };
    let stamp = &stem[stamp_start + 2..];
    if stamp.len() == ID_LEN + 1
        && stamp.ends_with(']')
        && stamp[..ID_LEN]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        stem[..stamp_start].to_string()
    } else {
        stem.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_ids_from_all_url_shapes() {
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=42s",
            "https://youtu.be/dQw4w9WgXcQ?si=abc",
            "youtu.be/dQw4w9WgXcQ",
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://m.youtube.com/watch?v=dQw4w9WgXcQ&pp=xyz",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
            "https://www.youtube.com/embed/dQw4w9WgXcQ",
            "https://www.youtube.com/live/dQw4w9WgXcQ",
            "dQw4w9WgXcQ",
        ] {
            assert_eq!(extract_video_id(url), Some("dQw4w9WgXcQ".into()), "{url}");
        }
    }

    #[test]
    fn rejects_non_video_urls() {
        for url in [
            "",
            "   ",
            "https://www.youtube.com/playlist?list=PL123",
            "https://www.youtube.com/watch",
            "https://www.youtube.com/watch?list=PL1&v=",
            "https://example.com/watch?v=dQw4w9WgXcQ",
            "https://youtube.com.evil.net/watch?v=dQw4w9WgXcQ",
            "https://vimeo.com/123456",
            "dQw4w9WgXc",
            "not a link",
        ] {
            assert_eq!(extract_video_id(url), None, "{url}");
        }
    }

    #[test]
    fn trims_the_id_stamp_from_downloaded_names() {
        let with_stamp = display_name(Path::new(
            "bardwave-library/Epic Tavern Song [dQw4w9WgXcQ].mp3",
        ));
        assert_eq!(with_stamp, "Epic Tavern Song");

        // Normal files keep their stem, stamp-like name or not.
        let manual = display_name(Path::new("battle [heavy].mp3"));
        assert_eq!(manual, "battle [heavy]");
        let plain = display_name(Path::new("storm.mp3"));
        assert_eq!(plain, "storm");
    }

    #[test]
    fn last_lines_keeps_the_tail_of_long_output() {
        let lines: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
        let out = last_lines(lines.iter().map(String::as_str), 20);
        assert!(out.ends_with("line 49"));
        assert!(out.chars().count() <= 30);
        assert!(out.starts_with("line 4"));
    }

    #[test]
    fn yt_dlp_asset_matches_the_release_layout() {
        // Every supported OS maps onto a real yt-dlp release asset and stores
        // it under the plain executable name find searches for.
        let (asset, name) = yt_dlp_asset().expect("this test build runs on a supported platform");
        assert!(asset.starts_with("yt-dlp"), "{asset}");
        let exec_names: [&str; 2] = ["yt-dlp", "yt-dlp.exe"];
        assert!(exec_names.contains(&name), "{name}");
    }

    #[test]
    fn tool_dirs_end_in_the_user_tools_folder() {
        let dirs = tool_dirs();
        assert!(
            !dirs.is_empty(),
            "the executable directory should always resolve"
        );
        // Both roots end in the same two segments, whichever base was chosen.
        let user = user_tools_dir().expect("env provides one of the data roots");
        assert!(user.ends_with(Path::new("Bardwave").join("tools")));
    }

    #[test]
    fn progress_marks_become_dialog_lines() {
        assert_eq!(
            progress_message("[download]  12.3% of    1.23MiB at  586.28KiB/s ETA 00:02"),
            Some("Downloading audio… 12.3%".to_string())
        );
        assert_eq!(progress_message("[download] Destination: video.m4a"), None);
        assert_eq!(
            progress_message("[ExtractAudio] Destination: video.mp3"),
            Some("Converting to MP3…".to_string())
        );
        assert_eq!(progress_message("ERROR: This video is unavailable"), None);
    }

    #[test]
    fn sanitizes_titles_into_creatable_windows_names() {
        assert_eq!(
            sanitize_windows_name("AC/DC: Back in *Black*"),
            "AC_DC_ Back in _Black_"
        );
        // Whitespace folds, trailing dots go away, blanks-only titles get a
        // usable fallback.
        assert_eq!(sanitize_windows_name("  spaced \t out  "), "spaced out");
        assert_eq!(sanitize_windows_name("trailing dots..."), "trailing dots");
        assert_eq!(sanitize_windows_name("   "), "video");
        assert_eq!(sanitize_windows_name(""), "video");
        // The name stays capped regardless of the title length.
        let long = "a".repeat(200);
        assert_eq!(sanitize_windows_name(&long).chars().count(), MAX_TITLE_LEN);
        // Device names are uncreatable on Windows in every namespace.
        assert_eq!(sanitize_windows_name("CON"), "_CON");
        assert_eq!(sanitize_windows_name("com4 song"), "_com4 song");
    }

    #[test]
    fn percent_signs_survive_the_output_template() {
        assert_eq!(escape_output_template("100% Live"), "100%% Live");
        assert_eq!(escape_output_template("plain name"), "plain name");
    }

    #[test]
    fn progress_reader_survives_non_utf8_bytes() {
        // A cp1251-style byte in the output must neither abort the reader nor
        // eat the following progress line: the old `lines()` loop died on the
        // first invalid byte, closed the pipe, and yt-dlp crashed writing into
        // it ("Errno 22 when flushing sys.stdout").
        let feed: &[u8] = b"\xff\xfe *garbled*\n[download]  42.0% of 1.0MiB\nstill \xB5 alive\n";
        let tail_store: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let store: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_store = store.clone();
        let sink: StatusSink = Arc::new(move |line| {
            if let Ok(mut lines) = sink_store.lock() {
                lines.push(line.to_string());
            }
        });
        let sink_ref: &(dyn Fn(&str) + Send + Sync) = &*sink;

        read_progress_lines(feed, tail_store.clone(), sink_ref);

        assert_eq!(store.lock().unwrap().len(), 1, "one progress line mapped");
        assert_eq!(
            tail_store.lock().unwrap().len(),
            3,
            "every line kept, lossy"
        );
    }
}

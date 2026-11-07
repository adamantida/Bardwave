# Agent.md — Bardwave (SoundShared)

> **Project:** Bardwave — free, open-source ambience, music and sound effects for tabletop RPG
> **Repo:** https://github.com/adamantida/Bardwave
> **Package:** `soundshared` v0.1.0
> **License:** MIT © 2026 adamantida

---

## 1. Project Overview

Bardwave is a desktop TTRPG soundboard that streams an ambient mix to players' browsers over a local network (LAN). The Game Master runs the desktop app (egui console), loads sounds, mixes them live, and broadcasts over WebSocket. Players open one URL in any browser and hear the mix — no accounts, no subscriptions, no cloud.

**Core architecture:**
```
┌─────────────────────── Bardwave.exe ────────────────────────┐
│   egui console ──AudioCommand──▶  audio engine              │
│   (sounds, groups,     ◀─status─  ├─ decoder  (Symphonia)   │
│    server panel)                  ├─ mixer    (48k/stereo)  │
│                                   └─ cpal output ─▶ speakers │
│                                          │                  │
│                                          │ MixedOutput      │
│                                          ▼                  │
│                              broadcaster (tokio broadcast)  │
│                                          │                  │
│                              axum HTTP + WebSocket          │
└──────────────────────────────────────────┼──────────────────┘
                                           │ ws://<lan-ip>:8080/ws/audio?k=<key>
                                  ┌────────┼────────┐
                                  ▼        ▼        ▼
                              🧝 browser  🧙 browser  🧟 browser
                              (Web Audio API → speakers)
```

**Crates/Modules:**
| Module | Responsibility |
|--------|---------------|
| `src/audio` | Decoding (Symphonia), mixing, resampling (48k stereo f32), cpal output |
| `src/network` | axum HTTP/WebSocket server, broadcaster, embedded listener page, local-IP detection |
| `src/gui` | egui console: theme, widgets, sound list, groups, server panel, dialogs |
| `src/config` | `config.toml` schema and persistence |
| `src/error` | Typed error types (`SoundSharedError`) |

**Key dependencies:** egui, eframe, cpal, symphonia, tokio, axum, tokio-tungstenite, rusty-opus, serde, tracing, anyhow, thiserror, uuid, crossbeam-channel.

**Build requirements:** Rust 1.85+, Windows 10/11 (primary target; local-IP detection is Windows-specific today).

---

## 2. Agent Team Structure

This project uses an **AgentTeams** system. Agents are assigned specific roles and tasks. The Team Lead ("lead") coordinates all work.

### Current Team Roles (from historical context)

| Role | Responsibility |
|------|---------------|
| **Software Architect** | Cross-crate integration, type definitions, error types, repository patterns |
| **Frontend Engineer** | Tauri/frontend screens, HTML/JS connection to backend commands |
| **UI Engineer** | TUI states, adaptive layout, widgets, screens, keyboard loop |
| **Desktop App Engineer** | Dashboard implementation, Tauri command integration |
| **Test Automation Engineer** | Test plans, quality gates, verification |
| **Rust Engineer** | Splash screen, UI implementation, `cargo check` verification |

> **Note:** This project was previously scoped as a multi-crate Mesh VoidLink/Tunnel project (with `crates/core`, `crates/app`, `crates/tui`, etc.). The current repository is a single-crate Bardwave/SoundShared project. Agent assignments should reflect the actual repo structure.

---

## 3. Task Management Protocol

All work flows through the **AgentTeams** task board. Follow this exact protocol:

### 3.1 Claiming a Task

1. Call `agent_teams_claim_task` for the task — it returns an `attempt_id`.
2. Include `attempt_id` in **every** `agent_teams_update_task` call.
3. If the claim is rejected as stale, stop work — the task was reassigned.

### 3.2 Task Lifecycle

```
pending → in_progress → completed (or failed)
   ↑          ↓              ↓
   └── release ←───────────┘
```

- **`claimed` cannot jump to `completed`** — mark `in_progress` first, then `completed` or `failed`.
- Include `attempt_id` on every update call.
- After work and verification finish, call `agent_teams_update_task` immediately — do not wait for captain confirmation.
- Do not approve your own implementation.
- Mail is not a formal next review.

### 3.3 Completion Output Requirements

For **implementation/repair/verification/integration** tasks, include structured fields:

- `acceptanceResults`: array of `{ status: "passed" | "failed", description }`
- `commandsRun`: array of commands executed and their results
- `inScopeChangedPaths`: list of files modified (implementation/repair only)
- `evidenceNote`: concise summary

For **review/requirements** tasks:
- `verdict: "pass"` to complete; `needs_revision` or `reject` must fail with findings.

### 3.4 Reporting

After completion, send a message to your parent agent with:
- `source_task_id` and `source_attempt_id`
- A self-contained result summary

The parent shares your workspace but does not automatically receive your transcript, tool output, or reasoning.

---

## 4. Development Guidelines

### 4.1 Code Style

- **Typed commands over shared mutable state** — no shared mutable soup between GUI, audio engine, and network.
- **No blocking work on the render thread** — audio and network run on dedicated threads.
- **Error handling:** Use `anyhow::Result` and `thiserror` for typed errors. Panics should be caught by the global panic hook in `main.rs`.
- **Logging:** Use `tracing` with `tracing-subscriber`. Log levels: trace, debug, info, warn, error. `RUST_LOG` env var overrides `config.toml` logging level.
- **Formatting:** Run `cargo fmt --check` before submitting. All code must be `rustfmt`-clean.

### 4.2 Build & Test Commands

```bash
# Full test suite (150+ tests)
cargo test --all-targets

# Lint
cargo clippy --all-targets

# Format check
cargo fmt --check

# Release build
cargo build --release

# Run the app
cargo run --release
```

### 4.3 Configuration

`config.toml` lives next to the executable:

```toml
[server]
host = "0.0.0.0"
port = 8080
upnp = true
access_token = "..."   # key listeners need in the link (/?k=...); "" = open

[audio]
buffer_size = 480
sample_rate = 48000
channels = 2
master_volume = 0.8

[logging]
level = "info"
file_logging = false

[gui]
reduce_motion = false

[library]
```

### 4.4 Audio Pipeline

- Decode: Symphonia (WAV, FLAC, MP3, OGG-Vorbis, AAC/MP4)
- Mix: 48 kHz / stereo f32
- Output: cpal (local speakers) + WebSocket broadcast
- Network encode: Opus (~64 kbit/s VBR), 10 ms frames (480 samples/channel)

### 4.5 WebSocket Wire Format

**Binary frame:**
| Offset | Size | Field |
|--------|------|-------|
| 0 | 4 bytes | `seq_no` — u32 LE, monotonically increasing |
| 4 | 4 bytes | `server_ms` — u32 LE, stream time since broadcaster start |
| 8 | variable | Opus packet (one 10 ms frame) |

**Format announcement (text message on first frame):**
```json
{"type":"format","data":{"sample_rate":48000,"channels":2,"format":"opus","frame_size":480}}
```

---

## 5. File Structure

```
Bardwave/
├── Agent.md                          # This file — agent team documentation
├── Cargo.toml                        # Workspace + package manifest
├── README.md                         # User-facing documentation
├── OPUS_MIGRATION.md                 # Opus migration notes
├── LICENSE                           # MIT license
├── assets/
│   └── README.md
├── src/
│   ├── main.rs                       # Entry point: audio engine, network server, GUI
│   ├── lib.rs                        # Library root, VERSION constant
│   ├── config.rs                     # config.toml schema & persistence
│   ├── error.rs                      # SoundSharedError types
│   ├── audio/
│   │   ├── mod.rs
│   │   ├── engine.rs                 # AudioEngine: decode, mix, output
│   │   ├── decoder.rs                # Symphonia decoding
│   │   ├── mixer.rs                  # 48k stereo mixing
│   │   └── ...
│   ├── network/
│   │   ├── mod.rs
│   │   ├── server.rs                 # axum router, ServerHandle, ServerState
│   │   ├── broadcast.rs              # tokio broadcast, AudioBroadcaster
│   │   ├── websocket.rs              # WebSocket session handling
│   │   ├── local_ip.rs               # LAN IP detection (Windows-specific)
│   │   ├── opus_wire.rs              # Opus wire format
│   │   └── static/
│   │       ├── index.html            # Embedded listener page
│   │       ├── app.js                # WebSocket client + Web Audio API
│   │       ├── style.css
│   │       └── opus-decoder.min.js
│   ├── gui/
│   │   ├── mod.rs
│   │   ├── app.rs                    # egui App implementation
│   │   ├── types.rs                  # AppState, AudioCommand, ServerCommand, ServerStatus
│   │   ├── theme.rs                  # egui theme
│   │   ├── widgets.rs                # Custom egui widgets
│   │   ├── sound_list.rs             # Sound list widget
│   │   ├── groups.rs                 # Group management
│   │   ├── server_panel.rs           # Server control panel
│   │   ├── top_bar.rs                # Top status bar
│   │   ├── dialog.rs                 # Dialogs (new/rename/delete group)
│   │   └── motion.rs                 # Reduced-motion support
│   └── ...
└── target/                           # Build output (gitignored)
```

---

## 6. Communication Protocols

### 6.1 Inter-Component Commands

**AudioCommand** (GUI → Audio Engine):
- Play, stop, pause sounds
- Set volume, loop, group assignment
- Add/remove sounds and groups

**ServerCommand** (GUI → Network Server):
- `Start { response }` — start the listener server
- `Stop { response }` — stop the listener server
- `SetBind { host, port, response }` — change bind address/port
- `GetStatus { response }` — query server status

**ServerStatus:**
```rust
struct ServerStatus {
    running: bool,
    address: Option<String>,
    listener_count: usize,
}
```

### 6.2 Agent-Team Messaging

- Use `send_message({ target: "<teammate name>", message: "..." })` to message teammates.
- A running target receives it at the nearest step boundary; an inactive target starts/resumes a turn.
- Use `list_agents({})` to find teammates and their availability.
- Parent agent ID must receive results before finishing a task.

---

## 7. Quality Gates

| Check | Command | Required |
|-------|---------|----------|
| Format | `cargo fmt --check` | Must pass (0 diffs) |
| Clippy | `cargo clippy --all-targets` | Must pass (0 warnings) |
| Test | `cargo test --all-targets` | All tests must pass |
| Build | `cargo build --release` | Must compile cleanly |

**Known pre-existing issues** (do not introduce new ones):
1. `crates/storage` module path mismatch (`pub mod activity;` vs `repositories/activity.rs`) — blocks `cargo fmt --check --workspace`
2. Windows schannel SSL `SEC_E_NO_CREDENTIALS` — blocks `crates.io` downloads for crates with external dependencies

---

## 8. Onboarding for New Agents

1. Read this `Agent.md` and `README.md` thoroughly.
2. Build the project: `cargo build --release`.
3. Run the tests: `cargo test --all-targets`.
4. Run the app: `cargo run` — verify the GUI opens and the server panel works.
5. Check available teammates: `list_agents({})`.
6. Claim a task from the shared task board following the protocol in Section 3.
7. On completion, update the task and notify the parent agent.

---

## 9. Revision History

| Version | Date | Changes | Author |
|---------|------|---------|--------|
| 1.0 | Initial | Agent team documentation for Bardwave | Lead |

---
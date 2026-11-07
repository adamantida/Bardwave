//! Bardwave (SoundShared) - Main Entry Point
//!
//! Integrates audio engine, network server, and GUI into a single application.

// A release build is a GUI program: no black console window next to the
// console. When it is started from a terminal (e.g. `--server-only`) it
// re-attaches to that terminal, see `attach_parent_console`.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Sender};
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

use soundshared::{
    audio::{AudioEngine, MixedOutput, TARGET_CHANNELS, TARGET_SAMPLE_RATE},
    config::{parse_bind_host, Config},
    error::{Result, SoundSharedError},
    gui::{
        run_gui,
        types::{AppState, AudioCommand, ServerCommand, ServerStatus},
    },
    network::{
        broadcast::{
            AudioBroadcaster, AudioFrame, BroadcastError, BroadcasterConfig, BroadcasterHandle,
        },
        local_ip::detect_for_bind,
        server::{build_router, listen_url, Router, ServerConfig, ServerHandle, ServerState},
        upnp::{self, PortMapping, UpnpStatus},
        websocket::WebSocketConfig,
    },
};

// Audio buffer type for inter-component communication (from mixer)
type MixedOutputBuffer = MixedOutput;

/// Audio processor that receives mixed audio from engine and forwards to broadcaster
struct AudioProcessor {
    /// Receiver for mixed audio buffers from audio engine
    audio_rx: Receiver<MixedOutputBuffer>,
    /// Broadcaster handle to send audio to listeners
    broadcaster: BroadcasterHandle,
    /// Shutdown signal (crossbeam)
    shutdown_rx: Receiver<()>,
}

impl AudioProcessor {
    fn new(
        audio_rx: Receiver<MixedOutputBuffer>,
        broadcaster: BroadcasterHandle,
        shutdown_rx: Receiver<()>,
    ) -> Self {
        Self {
            audio_rx,
            broadcaster,
            shutdown_rx,
        }
    }

    fn run(&mut self) {
        info!("Audio processor started");
        loop {
            crossbeam_channel::select! {
                recv(self.audio_rx) -> msg => {
                    match msg {
                        Ok(mixed) => {
                            // Convert MixedOutput to AudioFrame for broadcaster.
                            // The mix pipeline's own constants, so a change of
                            // rate or channel count cannot drift apart here.
                            let frame = AudioFrame::new(
                                mixed.buffer,
                                TARGET_SAMPLE_RATE,
                                TARGET_CHANNELS as u16,
                                mixed.seq_no,
                            );
                            if let Err(e) = self.broadcaster.send_audio(frame) {
                                // Channel full - backpressure, drop frame
                                if !matches!(e, BroadcastError::ChannelFull) {
                                    warn!("Failed to broadcast audio: {}", e);
                                }
                            }
                        }
                        Err(_) => {
                            info!("Audio channel closed, stopping processor");
                            break;
                        }
                    }
                }
                recv(self.shutdown_rx) -> _ => {
                    info!("Shutdown signal received, stopping audio processor");
                    break;
                }
            }
        }
        info!("Audio processor stopped");
    }
}

/// Bind the listener and spawn the task that serves it.
///
/// Split out of the command loop so a bind failure (or a failure to read the
/// bound address) is a value the loop can report, instead of an early return
/// that silently kills the whole server thread.
async fn start_listener(
    app: &Router,
    bind_addr: std::net::IpAddr,
    bind_port: u16,
    shutdown_tx: &broadcast::Sender<()>,
) -> std::result::Result<(String, tokio::task::JoinHandle<()>), String> {
    let addr = std::net::SocketAddr::new(bind_addr, bind_port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| e.to_string())?;
    let actual_addr = listener.local_addr().map_err(|e| e.to_string())?;
    info!("Network server listening on {}", actual_addr);

    // Advertise somewhere the listener is actually reachable, recomputed on
    // every Start so an edited bind address is reflected in the URL the GUI
    // displays and copies. The family follows the bind: a socket on 0.0.0.0
    // serves IPv4 and must not advertise an IPv6 address.
    let detected_ip = detect_for_bind(bind_addr);
    let url = listen_url(bind_addr, actual_addr, detected_ip);

    let app_clone = app.clone();
    let mut shutdown_rx_for_server = shutdown_tx.subscribe();
    let task = tokio::spawn(async move {
        info!(
            "🌐 Server task started, beginning to serve on {}",
            actual_addr
        );
        // Opus packets are ~100 bytes every 10 ms: with Nagle's algorithm on,
        // Windows holds them back and sends bursts, which drains the
        // listener's jitter buffer and makes the stream stutter.
        let _ = axum::serve(
            listener,
            app_clone.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .tcp_nodelay(true)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx_for_server.recv().await;
            info!("🌐 Shutdown signal received, stopping server...");
        })
        .await;
        info!("🌐 Server task ended");
    });

    Ok((url, task))
}

/// Signal the running listener and wait for it to finish.
///
/// The wait is bounded on purpose. axum finishes a graceful shutdown only once
/// every open connection is gone, and a browser tab can hold a WebSocket open
/// indefinitely; waiting on that without a deadline is what used to freeze the
/// command loop (and the console, still reporting Live) forever.
async fn stop_listener(shutdown_tx: &broadcast::Sender<()>, task: tokio::task::JoinHandle<()>) {
    let _ = shutdown_tx.send(());
    let mut task = task;
    if tokio::time::timeout(Duration::from_secs(2), &mut task)
        .await
        .is_err()
    {
        warn!("Listener did not finish shutting down in time; detaching it");
        task.abort();
    }
}

/// Server manager that runs the network server in a Tokio runtime
/// Server does NOT auto-start - only starts when GUI sends Start command
fn run_server_and_get_handle(
    config: ServerConfig,
    server_cmd_rx: Receiver<ServerCommand>,
    listener_count_tx: Sender<usize>,
    server_handle_tx: Sender<ServerHandle>,
    upnp_status: Arc<Mutex<UpnpStatus>>,
    network_notes: Arc<Mutex<Vec<String>>>,
    extra_routes: Option<Router>,
) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create Tokio runtime");

    let result = rt.block_on(async {
        // Kept for the whole run: every listener session subscribes to this
        // signal, so a Stop reaches the browsers as well as the accept loop.
        let (shutdown_tx, _) = broadcast::channel(1);

        // Create broadcaster
        let (broadcaster_handle, broadcaster) = AudioBroadcaster::new(config.broadcaster_config.clone());

        // The bind target is live state, not a snapshot: the GUI can change the
        // CONFIGURATION port / bind address while the server is down, and the
        // next Start has to use the new values.
        let mut bind_addr = config.bind_addr;
        let mut bind_port = config.port;
        // URL of the running server; `None` while the socket is down.
        let mut server_url: Option<String> = None;
        // Router port mapping, kept alive while the server runs.
        let mut upnp_mapping: Option<PortMapping> = None;
        let mut upnp_poll = tokio::time::interval(Duration::from_secs(1));
        upnp_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // One router definition, shared with NetworkServer: the app used to
        // hand-build a second copy of this exact route table.
        let state = ServerState {
            broadcaster: broadcaster_handle.clone(),
            websocket_config: config.websocket_config.clone(),
            shutdown_tx: shutdown_tx.clone(),
            access: config.access.clone(),
        };

        let app = match extra_routes {
            Some(routes) => build_router(state).merge(routes),
            None => build_router(state),
        };

        // Create server handle (URL is None until Start command)
        let shutdown_tx_for_handle = shutdown_tx.clone();
        let handle = ServerHandle {
            shutdown_tx: shutdown_tx_for_handle,
            url: Arc::new(tokio::sync::Mutex::new(None)),
            broadcaster: broadcaster_handle.clone(),
        };

        // Send handle to audio processor
        let _ = server_handle_tx.send(handle.clone());

        // Spawn broadcaster task once — it runs continuously regardless of server state
        let broadcaster_task = tokio::spawn(broadcaster.run());

        // Subscribe to listener count
        let mut count_rx = broadcaster_handle.subscribe_count();

        // Convert crossbeam receiver to async-friendly polling (non-blocking)
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<ServerCommand>(32);
        let cmd_rx_crossbeam = server_cmd_rx.clone();
        tokio::spawn(async move {
            loop {
                if let Ok(cmd) = cmd_rx_crossbeam.try_recv() {
                    if cmd_tx.send(cmd).await.is_err() {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });

        // Server state — does NOT auto-start. Waits for GUI Start command.
        let mut server_running = false;
        let mut listener_count = 0;
        let mut server_task: Option<tokio::task::JoinHandle<()>> = None;

        info!("🌐 Server initialized, waiting for Start command");
        info!(
            "Server will bind {}:{} on the next Start command",
            bind_addr, bind_port
        );

        loop {
            tokio::select! {
                // Handle server commands from GUI
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(ServerCommand::Start { response }) => {
                            if server_running {
                                let _ = response.send(Err(anyhow::anyhow!("Server already running")));
                            } else {
                                // Bind listener and start serving ONLY on Start command.
                                // Reported through the response channel, not by
                                // bubbling up: a bind failure must not kill the
                                // server thread.
                                let started =
                                    start_listener(&app, bind_addr, bind_port, &shutdown_tx).await;

                                match started {
                                    Ok((url, task)) => {
                                        server_running = true;
                                        listener_count = 0;
                                        server_task = Some(task);

                                        server_url = Some(url.clone());
                                        *handle.url.lock().await = Some(url.clone());
                                        let _ = response.send(Ok(url.clone()));
                                        info!("Server started at {}", url);

                                        // Players on the LAN and on the internet
                                        // both need an inbound firewall rule.
                                        // `netsh` blocks, so it runs off this
                                        // thread, which also carries the audio.
                                        let port = bind_port;
                                        let notes = Arc::clone(&network_notes);
                                        *notes.lock().unwrap() = soundshared::network::local_ip::vpn_warning(
                                            detect_for_bind(bind_addr),
                                        )
                                        .into_iter()
                                        .collect();
                                        tokio::task::spawn_blocking(move || {
                                            if !upnp::allow_through_firewall(port) {
                                                notes.lock().unwrap().push(format!(
                                                    "Firewall: port {port} not opened - allow Bardwave for public networks or run as admin"
                                                ));
                                            }
                                        });

                                        // Port forwarding runs in the background:
                                        // the LAN URL above is never held up by
                                        // a slow or absent router.
                                        if config.upnp {
                                            upnp_mapping = upnp::start(bind_addr, bind_port);
                                        }
                                        *upnp_status.lock().unwrap() = if upnp_mapping.is_some() {
                                            UpnpStatus::Searching
                                        } else {
                                            UpnpStatus::Off
                                        };
                                    }
                                    Err(message) => {
                                        error!("Failed to start listener: {}", message);
                                        let _ = response.send(Err(SoundSharedError::Other(message).into()));
                                    }
                                }
                            }
                        }
                        Some(ServerCommand::Stop { response }) => {
                            if server_running {
                                server_running = false;

                                if let Some(mapping) = upnp_mapping.take() {
                                    upnp::release(mapping).await;
                                }
                                *upnp_status.lock().unwrap() = UpnpStatus::Off;
                                network_notes.lock().unwrap().clear();

                                // Tell the listener (and every session it holds
                                // open) to stop, then wait for it with a deadline.
                                // Every listener session closes its socket on this
                                // signal, so the wait is short; the deadline is the
                                // backstop that keeps one stubborn client from
                                // freezing the command loop.
                                if let Some(task) = server_task.take() {
                                    stop_listener(&shutdown_tx, task).await;
                                }

                                server_url = None;
                                listener_count = 0;
                                *handle.url.lock().await = None;
                                let _ = listener_count_tx.send(0);
                                let _ = response.send(Ok(()));
                                info!("Server stopped");
                            } else {
                                let _ = response.send(Ok(()));
                            }
                        }
                        Some(ServerCommand::SetBind { host, port, response }) => {
                            if server_running {
                                // The socket is already bound; the new values
                                // only apply to the next Start.
                                let _ = response.send(Err("Stop the server first".to_string()));
                            } else {
                                match parse_bind_host(&host) {
                                    None => {
                                        let _ = response.send(Err(format!(
                                            "Not an IP address: {}",
                                            host
                                        )));
                                    }
                                    Some(_) if port == 0 => {
                                        let _ = response.send(Err(
                                            "Port must be 1-65535".to_string(),
                                        ));
                                    }
                                    Some(ip) => {
                                        bind_addr = ip;
                                        bind_port = port;
                                        info!(
                                            "Server bind address set to {}:{}",
                                            bind_addr, bind_port
                                        );
                                        let _ = response.send(Ok(()));
                                    }
                                }
                            }
                        }
                        Some(ServerCommand::GetStatus { response }) => {
                            let status = ServerStatus {
                                running: server_running,
                                address: server_url.clone(),
                                listener_count,
                            };
                            let _ = response.send(status);
                        }
                        None => break,
                    }
                }

                // Follow the router mapping: it can be granted, renewed or lost
                // at any time while the server runs.
                _ = upnp_poll.tick(), if upnp_mapping.is_some() => {
                    if let Some(mapping) = &upnp_mapping {
                        let next = mapping.status();
                        let mut current = upnp_status.lock().unwrap();
                        if *current != next {
                            match &next {
                                UpnpStatus::Mapped { external, url } => {
                                    info!("Router forwards {external} -> port {bind_port}; internet URL {url}");
                                }
                                UpnpStatus::DoubleNat { external } => warn!(
                                    "Router forwards {external}, but that address is private: \
                                     another NAT (CGNAT / provider modem) is in front of the router, \
                                     so the port is not reachable from the internet"
                                ),
                                UpnpStatus::Unavailable(reason) => warn!(
                                    "No router port forwarding yet ({reason}); retrying in the background"
                                ),
                                UpnpStatus::Searching | UpnpStatus::Off => {}
                            }
                            *current = next;
                        }
                    }
                }

                // Update listener count from broadcaster
                count = count_rx.recv() => {
                    if let Ok(count) = count {
                        listener_count = count;
                        let _ = listener_count_tx.send(listener_count);
                    }
                }
            }
        }

        // Cleanup on shutdown. The broadcaster is detached instead of awaited:
        // it lives as long as the audio engine holds its handle, so waiting for
        // it here would block this thread for the rest of the run.
        if let Some(mapping) = upnp_mapping.take() {
            upnp::release(mapping).await;
        }
        if let Some(task) = server_task.take() {
            stop_listener(&shutdown_tx, task).await;
        }
        broadcaster_task.abort();

        Ok::<(), SoundSharedError>(())
    });

    result
}

fn main() -> Result<()> {
    // Initialize panic hook for clean error reporting
    std::panic::set_hook(Box::new(|panic_info| {
        let msg = match panic_info.payload().downcast_ref::<&str>() {
            Some(s) => *s,
            None => match panic_info.payload().downcast_ref::<String>() {
                Some(s) => s.as_str(),
                None => "Unknown panic",
            },
        };
        let location = panic_info
            .location()
            .map(|l| format!(" at {}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default();
        error!("PANIC: {}{}", msg, location);

        if let Ok(bt) = std::panic::catch_unwind(backtrace::Backtrace::new) {
            error!("Backtrace:\n{:?}", bt);
        }
    }));

    // Check for --server-only flag to run without GUI
    let args: Vec<String> = std::env::args().collect();
    let server_only = args.iter().any(|a| a == "--server-only");

    attach_parent_console(server_only);

    // Load configuration. A broken config.toml used to end the process
    // silently (there is no console to print to), which looked like the app
    // simply refusing to start. Say what is wrong instead; never fall back to
    // defaults here, because the next save would overwrite the library.
    let mut config = match Config::load() {
        Ok(config) => config,
        Err(e) => {
            if !server_only {
                rfd::MessageDialog::new()
                    .set_level(rfd::MessageLevel::Error)
                    .set_title("Bardwave")
                    .set_description(format!(
                        "config.toml could not be read:\n\n{e}\n\nFix the file (or move it away to start with defaults) and launch Bardwave again."
                    ))
                    .set_buttons(rfd::MessageButtons::Ok)
                    .show();
            }
            eprintln!("Error: {e}");
            return Err(e);
        }
    };
    info!(
        "Configuration loaded: server={}, audio_buffer={}, log_level={}",
        config.server_addr(),
        config.audio.buffer_size,
        config.logging.level
    );

    // Initialize logging
    init_logging(&config)?;

    // The stream is guarded by an access key carried in the shared link. A
    // config without one gets a fresh key (and keeps it); `access_token = ""`
    // explicitly opens the stream to anyone who can reach the port.
    if config.server.access_token.is_none() {
        config.server.access_token = Some(soundshared::network::generate_key());
        if let Err(e) = config.save() {
            warn!("Could not save the new access key to config.toml: {}", e);
        }
    }
    let access = soundshared::network::AccessGuard::new(config.server.access_token.clone());
    if access.current().is_none() {
        warn!("Listener stream is open: access_token is empty in config.toml");
    }

    info!("Starting SoundShared v{}", soundshared::VERSION);

    // Create channels for inter-component communication
    let (audio_cmd_tx, audio_cmd_rx) = bounded::<AudioCommand>(32);
    let (server_cmd_tx, server_cmd_rx) = bounded::<ServerCommand>(32);
    let (shutdown_tx, shutdown_rx) = bounded::<()>(1);
    let (listener_count_tx, listener_count_rx) = bounded::<usize>(1);
    let (server_handle_tx, server_handle_rx) = bounded::<ServerHandle>(1);
    let server_cmd_tx_for_start = server_cmd_tx.clone();

    // Create audio engine
    let audio_engine = AudioEngine::new().map_err(|e| {
        SoundSharedError::Audio(soundshared::error::AudioError::Engine(e.to_string()))
    })?;

    // Set master volume from config
    audio_engine
        .set_master_volume(config.audio.master_volume)
        .map_err(|e| {
            SoundSharedError::Audio(soundshared::error::AudioError::Mixer(e.to_string()))
        })?;

    // `[audio]`: how the desktop sound card is opened (the mix and the network
    // stream stay 48 kHz stereo).
    audio_engine.set_device_prefs(soundshared::audio::engine::DevicePrefs::from_audio_config(
        &config.audio,
    ));

    // Get the broadcast output receiver for WebSocket streaming
    let broadcast_output_rx = audio_engine.broadcast_output_receiver();

    // Wrap audio engine in Arc<Mutex> for sharing with GUI (main thread)
    #[allow(clippy::arc_with_non_send_sync)]
    let audio_engine_shared = Arc::new(Mutex::new(Some(audio_engine)));

    // Create network server config. The listener page is embedded in the binary
    // (build_router), so there is no static directory to point at any more.
    let server_config = ServerConfig {
        port: config.server.port,
        bind_addr: parse_bind_host(&config.server.host)
            .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::new(0, 0, 0, 0))),
        upnp: config.server.upnp,
        broadcaster_config: BroadcasterConfig::default(),
        websocket_config: WebSocketConfig::default(),
        access: access.clone(),
    };

    // Create shared app state (for GUI)
    let app_state = AppState::new(
        audio_cmd_tx,
        server_cmd_tx,
        Arc::new(Mutex::new(0)),
        Arc::new(Mutex::new(config.clone())),
    )
    .with_access(access.clone());

    // Spawn server thread (does NOT auto-start - waits for Start command from GUI).
    // The thread learns about process shutdown from its own Stop command / the
    // process exit; the crossbeam shutdown channel only carries the audio
    // processor's signal, which is why this call never took it.
    let listener_count_tx_clone = listener_count_tx.clone();
    let server_cmd_rx_clone = server_cmd_rx.clone();
    let server_config_clone = server_config.clone();
    let server_handle_tx_clone = server_handle_tx.clone();
    let upnp_status = app_state.upnp_status.clone();
    let network_notes = app_state.network_notes.clone();
    // `--server-only` has no console window: the library is played through a
    // local control page instead (see network::control).
    let (control_tx, control_rx) =
        crossbeam_channel::unbounded::<soundshared::network::control::ControlCommand>();
    let control_routes = server_only.then(|| soundshared::network::control::router(control_tx));
    thread::spawn(move || {
        if let Err(e) = run_server_and_get_handle(
            server_config_clone,
            server_cmd_rx_clone,
            listener_count_tx_clone,
            server_handle_tx_clone,
            upnp_status,
            network_notes,
            control_routes,
        ) {
            error!("Server error: {}", e);
        }
    });

    // Spawn listener count updater for GUI
    let listener_count_state = app_state.listener_count.clone();
    thread::spawn(move || {
        for count in listener_count_rx {
            *listener_count_state.lock().unwrap() = count;
        }
    });

    // Wait for server handle to be available, then spawn audio processor
    let broadcast_output_rx_for_processor = broadcast_output_rx;
    let processor_shutdown_rx = shutdown_rx.clone();
    thread::spawn(move || {
        // Wait for server handle
        let server_handle = match server_handle_rx.recv() {
            Ok(handle) => handle,
            Err(_) => {
                info!("Server handle channel closed, audio processor not started");
                return;
            }
        };

        let broadcaster = server_handle.broadcaster().clone();
        let mut processor = AudioProcessor::new(
            broadcast_output_rx_for_processor,
            broadcaster,
            processor_shutdown_rx,
        );
        processor.run();
    });

    // Start the audio engine output stream (on main thread). If no local
    // output device is available, keep streaming to network listeners.
    {
        let engine_guard = audio_engine_shared.lock().unwrap();
        if let Some(engine) = engine_guard.as_ref() {
            if let Err(e) = engine.start_output() {
                warn!(
                    "Failed to start local audio output: {}. Falling back to headless streaming.",
                    e
                );
                // Carry the reason the device could not be opened, so the status
                // strip can say *why* the desktop is silent instead of just
                // reporting "headless".
                let reason = format!("local output unavailable: {e}");
                if let Err(headless_err) = engine.start_headless_output_with_reason(reason) {
                    error!("Failed to start headless audio engine: {}", headless_err);
                }
            }
        }
    }

    if server_only {
        info!("Running in server-only mode (no GUI). Press Ctrl+C to stop.");

        // Load the saved library so there is something to play.
        let mut board = {
            let guard = audio_engine_shared.lock().unwrap();
            let engine = guard.as_ref().expect("audio engine");
            soundshared::network::control::Board::load(engine, &config.library)
        };
        info!("Library loaded: {} sound(s)", board.len());

        // There is no GUI to press "Start", so start the listener server here.
        let (response_tx, response_rx) = bounded::<anyhow::Result<String>>(1);
        if let Err(e) = server_cmd_tx_for_start.send(ServerCommand::Start {
            response: response_tx,
        }) {
            error!("Failed to request server start: {}", e);
        } else {
            match response_rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Ok(url)) => {
                    info!("Listener server available at {}", access.share_url(&url));
                    if let Some(local) = soundshared::network::loopback_url(&url) {
                        info!("Control page (this PC only): {local}/control");
                    }
                }
                Ok(Err(e)) => error!("Failed to start listener server: {}", e),
                Err(e) => error!("Timed out waiting for listener server to start: {}", e),
            }
        }

        // Wait for Ctrl+C. The process exits after this, which tears down the
        // server and audio threads.
        let (ctrlc_tx, ctrlc_rx) = bounded::<()>(1);
        ctrlc::set_handler(move || {
            let _ = ctrlc_tx.send(());
        })
        .expect("Error setting Ctrl+C handler");

        // Run control actions on this thread (the engine is not Send) until
        // Ctrl+C.
        loop {
            crossbeam_channel::select! {
                recv(ctrlc_rx) -> _ => break,
                recv(control_rx) -> command => {
                    let Ok(command) = command else { break };
                    let guard = audio_engine_shared.lock().unwrap();
                    let result = match guard.as_ref() {
                        Some(engine) => board.handle(engine, command.action),
                        None => Err("audio engine stopped".to_string()),
                    };
                    let _ = command.reply.send(result);
                }
            }
        }

        info!("Shutting down...");
        stop_server_and_wait(&server_cmd_tx_for_start);
        let _ = shutdown_tx.send(());
        thread::sleep(Duration::from_millis(300));

        if let Some(engine) = audio_engine_shared.lock().unwrap().take() {
            if let Err(e) = engine.stop_output() {
                warn!("Error stopping audio engine: {}", e);
            }
        }

        info!("SoundShared stopped");
        Ok(())
    } else {
        // Run the GUI (blocks until window closes)
        // Pass audio_engine_shared and audio_cmd_rx so GUI can process commands on main thread
        info!("Starting GUI...");
        let gui_result = run_gui(app_state.clone(), audio_engine_shared.clone(), audio_cmd_rx);

        // Shutdown sequence
        info!("Shutting down...");
        stop_server_and_wait(&server_cmd_tx_for_start);
        let _ = shutdown_tx.send(());

        // Wait a bit for threads to clean up
        thread::sleep(Duration::from_millis(500));

        // Stop audio engine (on main thread)
        if let Some(engine) = audio_engine_shared.lock().unwrap().take() {
            if let Err(e) = engine.stop_output() {
                warn!("Error stopping audio engine: {}", e);
            }
        }

        info!("SoundShared stopped");
        gui_result.map_err(|e| SoundSharedError::Other(e.to_string()))
    }
}

/// Stops the listener through the server thread, so the router port mapping is
/// removed before the process exits.
fn stop_server_and_wait(server_cmd_tx: &Sender<ServerCommand>) {
    let (response_tx, response_rx) = bounded::<anyhow::Result<()>>(1);
    if server_cmd_tx
        .send(ServerCommand::Stop {
            response: response_tx,
        })
        .is_ok()
    {
        let _ = response_rx.recv_timeout(Duration::from_secs(5));
    }
}

/// Initialize logging based on configuration
///
/// With `logging.file_logging = true` the log also goes to `bardwave.log` next
/// to the executable — the only way to read it from a release build, which
/// has no console window.
fn init_logging(config: &Config) -> Result<()> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.logging.level));

    let log_file = if config.logging.file_logging {
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("bardwave.log")))
            .and_then(|path| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .ok()
            })
    } else {
        None
    };

    let builder = FmtSubscriber::builder()
        .with_env_filter(filter)
        .with_target(false)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true);

    let result = match log_file {
        Some(file) => {
            let file = Arc::new(Mutex::new(file));
            let writer = move || TeeWriter {
                file: Arc::clone(&file),
            };
            tracing::subscriber::set_global_default(builder.with_writer(writer).finish())
        }
        None => tracing::subscriber::set_global_default(builder.finish()),
    };
    result.map_err(|e| SoundSharedError::Other(format!("Failed to set global logger: {}", e)))?;

    Ok(())
}

/// Log writer for `file_logging`: stdout (when there is a console) plus the
/// log file.
struct TeeWriter {
    file: Arc<Mutex<std::fs::File>>,
}

impl std::io::Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stdout().write_all(buf);
        if let Ok(mut file) = self.file.lock() {
            file.write_all(buf)?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _ = std::io::stdout().flush();
        if let Ok(mut file) = self.file.lock() {
            file.flush()?;
        }
        Ok(())
    }
}

/// Re-attach stdout/stderr to the terminal that started us, if any.
///
/// The release build uses the GUI subsystem (no console window of its own),
/// so `soundshared.exe --server-only` started from cmd/PowerShell would
/// otherwise print nothing at all.
///
/// `--server-only` started from Explorer or a shortcut has no terminal at all;
/// it gets a console window of its own, so the log (and Ctrl+C to stop) is
/// there.
#[cfg(all(windows, not(debug_assertions)))]
fn attach_parent_console(server_only: bool) {
    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
        fn AllocConsole() -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    // SAFETY: plain Win32 calls without pointers; failure (no parent console)
    // is harmless and simply leaves the process without a console.
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 && server_only {
            AllocConsole();
        }
    }
}

#[cfg(not(all(windows, not(debug_assertions))))]
fn attach_parent_console(_server_only: bool) {}

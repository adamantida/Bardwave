//! The listener page's PWA assets: web app manifest, icons and the page that
//! links them. The router is the one the app serves, bound to an OS-chosen port.

use soundshared::network::broadcast::{AudioBroadcaster, BroadcasterConfig};
use soundshared::network::server::{build_router, ServerState};
use soundshared::network::websocket::WebSocketConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Response {
    status: u16,
    headers: String,
    body: Vec<u8>,
}

impl Response {
    fn header(&self, name: &str) -> Option<String> {
        let prefix = format!("{}:", name.to_ascii_lowercase());
        self.headers.lines().find_map(|line| {
            line.to_ascii_lowercase()
                .starts_with(&prefix)
                .then(|| line[prefix.len()..].trim().to_string())
        })
    }

    fn text(&self) -> String {
        String::from_utf8(self.body.clone()).expect("utf-8 body")
    }
}

async fn start_server() -> std::net::SocketAddr {
    let (broadcaster, task) = AudioBroadcaster::new(BroadcasterConfig::default());
    tokio::spawn(task.run());
    let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
    let state = ServerState {
        broadcaster,
        websocket_config: WebSocketConfig::default(),
        shutdown_tx,
        access: Default::default(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = build_router(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

async fn get(addr: std::net::SocketAddr, path: &str) -> Response {
    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read");

    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("end of headers");
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("status code");
    Response {
        status,
        headers: head,
        body: raw[split + 4..].to_vec(),
    }
}

/// Width and height from a PNG's IHDR chunk.
fn png_size(bytes: &[u8]) -> (u32, u32) {
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "PNG signature");
    assert_eq!(&bytes[12..16], b"IHDR");
    (
        u32::from_be_bytes(bytes[16..20].try_into().unwrap()),
        u32::from_be_bytes(bytes[20..24].try_into().unwrap()),
    )
}

#[tokio::test]
async fn manifest_is_served_with_its_media_type_and_valid_json() {
    let addr = start_server().await;
    let res = get(addr, "/manifest.webmanifest").await;

    assert_eq!(res.status, 200);
    assert_eq!(
        res.header("content-type").as_deref(),
        Some("application/manifest+json")
    );
    let manifest: serde_json::Value = serde_json::from_str(&res.text()).expect("manifest JSON");
    assert_eq!(manifest["short_name"], "Bardwave");
    assert_eq!(manifest["display"], "standalone");
    assert_eq!(manifest["start_url"], "/");
    assert!(manifest["icons"].as_array().is_some_and(|i| i.len() >= 2));
}

#[tokio::test]
async fn every_manifest_icon_is_a_png_of_the_declared_size() {
    let addr = start_server().await;
    let manifest: serde_json::Value =
        serde_json::from_str(&get(addr, "/manifest.webmanifest").await.text()).unwrap();

    let mut sizes = Vec::new();
    for icon in manifest["icons"].as_array().expect("icons") {
        let src = icon["src"].as_str().expect("icon src");
        let declared = icon["sizes"].as_str().expect("icon sizes");
        let res = get(addr, src).await;
        assert_eq!(res.status, 200, "{src}");
        assert_eq!(res.header("content-type").as_deref(), Some("image/png"));
        let (w, h) = png_size(&res.body);
        assert_eq!(format!("{w}x{h}"), declared, "{src}");
        sizes.push(w);
    }
    assert!(sizes.contains(&192) && sizes.contains(&512));
}

#[tokio::test]
async fn apple_touch_icon_is_a_180_pixel_png() {
    let addr = start_server().await;
    let res = get(addr, "/apple-touch-icon.png").await;
    assert_eq!(res.status, 200);
    assert_eq!(res.header("content-type").as_deref(), Some("image/png"));
    assert_eq!(png_size(&res.body), (180, 180));
}

#[tokio::test]
async fn manifest_carries_the_access_key_into_start_url() {
    let addr = start_server().await;

    let keyed: serde_json::Value =
        serde_json::from_str(&get(addr, "/manifest.webmanifest?k=abc123xyz").await.text()).unwrap();
    assert_eq!(keyed["start_url"], "/?k=abc123xyz");

    // Anything that is not a plain URL-safe key is not echoed.
    let hostile: serde_json::Value = serde_json::from_str(
        &get(addr, "/manifest.webmanifest?k=%22%3E%3Cscript%3E")
            .await
            .text(),
    )
    .unwrap();
    assert_eq!(hostile["start_url"], "/");
}

#[tokio::test]
async fn index_links_the_manifest_and_ios_meta_tags() {
    let addr = start_server().await;
    let res = get(addr, "/").await;
    assert_eq!(res.status, 200);
    let html = res.text();

    assert!(html.contains(r#"rel="manifest" href="/manifest.webmanifest""#));
    assert!(html.contains(r#"rel="apple-touch-icon" href="/apple-touch-icon.png""#));
    assert!(html.contains(r#"name="apple-mobile-web-app-capable""#));
    assert!(html.contains(r#"name="theme-color""#));
    assert!(html.contains("viewport-fit=cover"));
}

#[tokio::test]
async fn the_client_script_speaks_the_new_text_message_and_ignores_unknown_ones() {
    let addr = start_server().await;
    let js = get(addr, "/app.js").await.text();

    // Guards against the features being dropped by a careless edit; the
    // behaviour itself is covered by the browser-side checks in the PR notes.
    for needle in [
        "'now_playing'",
        "get_format",
        "navigator.wakeLock",
        "mediaSession",
        "localStorage",
    ] {
        assert!(js.contains(needle), "app.js lost {needle}");
    }
    // Unknown control messages fall into a silent default branch.
    assert!(js.contains("ignored on purpose"));
}

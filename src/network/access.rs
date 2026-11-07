//! Access key for the listener stream.
//!
//! Once UPnP forwards the port, anyone who learns (or scans) the address could
//! listen to the session. The stream is therefore guarded by a random key that
//! travels in the link the GM shares: `http://host:port/?k=<key>`. The page
//! itself and its static assets stay public (they reveal nothing); the audio
//! WebSocket and the status API require the key.
//!
//! The key lives in `[server] access_token` in `config.toml`. It is generated on
//! first start; an explicitly empty value (`access_token = ""`) turns the check
//! off. A new key can be issued from the console at any time; listeners that
//! connected with the old one are disconnected right away.

use std::sync::Arc;

use tokio::sync::watch;

/// Query parameter that carries the key.
pub const KEY_PARAM: &str = "k";

/// WebSocket close code sent when the key a listener used is no longer valid.
pub const CLOSE_KEY_REVOKED: u16 = 4001;

/// The current access key, shared by the console and the server.
///
/// `None` means the stream is open to anyone who can reach the port.
#[derive(Clone, Debug)]
pub struct AccessGuard {
    key: Arc<watch::Sender<Option<String>>>,
}

impl Default for AccessGuard {
    /// An open guard: no key required.
    fn default() -> Self {
        Self::new(None)
    }
}

impl AccessGuard {
    /// A guard for `key`; an empty key means "open".
    pub fn new(key: Option<String>) -> Self {
        let key = key.filter(|k| !k.trim().is_empty());
        let (tx, _) = watch::channel(key);
        Self { key: Arc::new(tx) }
    }

    /// The key currently in force, if any.
    pub fn current(&self) -> Option<String> {
        self.key.borrow().clone()
    }

    /// Replace the key. Sessions opened with the old key are closed.
    pub fn set(&self, key: Option<String>) {
        self.key.send_replace(key.filter(|k| !k.trim().is_empty()));
    }

    /// Whether `provided` opens the stream.
    pub fn allows(&self, provided: Option<&str>) -> bool {
        match self.key.borrow().as_deref() {
            None => true,
            Some(expected) => {
                provided.is_some_and(|p| constant_time_eq(p.as_bytes(), expected.as_bytes()))
            }
        }
    }

    /// Follow key changes (a session watches this to notice revocation).
    pub fn subscribe(&self) -> watch::Receiver<Option<String>> {
        self.key.subscribe()
    }

    /// `url` with the key appended as `/?k=…` (or unchanged when open).
    pub fn share_url(&self, url: &str) -> String {
        with_key(url, self.current().as_deref())
    }
}

/// `url` with `key` appended as `/?k=…`.
pub fn with_key(url: &str, key: Option<&str>) -> String {
    match key {
        Some(key) => format!("{}/?{KEY_PARAM}={key}", url.trim_end_matches('/')),
        None => url.to_string(),
    }
}

/// A fresh random key: 16 characters from `[a-z0-9]`, ~82 bits.
pub fn generate_key() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    // UUIDv4 comes from the OS CSPRNG (getrandom); two of them give plenty of
    // random bytes without another dependency.
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes
        .iter()
        // Version/variant nibbles of a UUID are fixed; skip those bytes.
        .enumerate()
        .filter(|(i, _)| i % 16 != 6 && i % 16 != 8)
        .map(|(_, b)| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .take(16)
        .collect()
}

/// Compare without leaking the position of the first mismatch through timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_guard_allows_everyone() {
        let guard = AccessGuard::default();
        assert!(guard.allows(None));
        assert!(guard.allows(Some("anything")));
        assert_eq!(
            guard.share_url("http://10.0.0.2:8080"),
            "http://10.0.0.2:8080"
        );
        // An empty key in the config means "open", not "the empty key".
        assert!(AccessGuard::new(Some(String::new())).allows(None));
    }

    #[test]
    fn keyed_guard_needs_the_exact_key() {
        let guard = AccessGuard::new(Some("abc123".into()));
        assert!(guard.allows(Some("abc123")));
        assert!(!guard.allows(None));
        assert!(!guard.allows(Some("abc12")));
        assert!(!guard.allows(Some("abc1234")));
        assert!(!guard.allows(Some("ABC123")));
        assert_eq!(
            guard.share_url("http://10.0.0.2:8080"),
            "http://10.0.0.2:8080/?k=abc123"
        );
    }

    #[test]
    fn a_new_key_revokes_the_old_one() {
        let guard = AccessGuard::new(Some("old".into()));
        let rx = guard.subscribe();
        guard.set(Some("new".into()));
        assert!(rx.has_changed().unwrap());
        assert!(!guard.allows(Some("old")));
        assert!(guard.allows(Some("new")));
    }

    #[test]
    fn generated_keys_are_url_safe_and_distinct() {
        let a = generate_key();
        let b = generate_key();
        assert_eq!(a.len(), 16);
        assert!(a
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
        assert_ne!(a, b);
    }
}

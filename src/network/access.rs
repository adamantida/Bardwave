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

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
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

/// Compare without leaking the position of the first mismatch or the length
/// of the expected value through timing: the loop always walks the longer of
/// the two inputs.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let len = a.len().max(b.len());
    let mut diff = a.len() ^ b.len();
    for i in 0..len {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= usize::from(x ^ y);
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// Game-master token (remote control panel)
// ---------------------------------------------------------------------------

/// Query parameter that carries the game-master token (`/control?g=…`,
/// `/ws/control?g=…`). Headers cannot be set on a link or a WebSocket from a
/// browser, so the parameter exists next to `Authorization: Bearer`.
pub const GM_PARAM: &str = "g";

/// WebSocket close code sent to a remote panel when the game-master token it
/// used was replaced.
pub const CLOSE_GM_TOKEN_REVOKED: u16 = 4003;

/// Failed attempts per client address before it is locked out for a while.
pub const GM_MAX_FAILURES: usize = 10;

/// The window in which [`GM_MAX_FAILURES`] failures lock a client out.
pub const GM_FAILURE_WINDOW: Duration = Duration::from_secs(60);

/// The game-master token, shared by the console and the server.
///
/// Unlike [`AccessGuard`] there is no "open" state: with no token (or an empty
/// one) nobody can authenticate remotely, and the panel is reachable from this
/// computer only. The listener key never opens the panel.
#[derive(Clone, Debug)]
pub struct GmGuard {
    token: Arc<watch::Sender<Option<String>>>,
}

impl Default for GmGuard {
    /// No token: remote access disabled.
    fn default() -> Self {
        Self::new(None)
    }
}

impl GmGuard {
    /// A guard for `token`; an empty token means "no remote access".
    pub fn new(token: Option<String>) -> Self {
        let token = token.filter(|t| !t.trim().is_empty());
        let (tx, _) = watch::channel(token);
        Self {
            token: Arc::new(tx),
        }
    }

    /// The token currently in force, if any.
    pub fn current(&self) -> Option<String> {
        self.token.borrow().clone()
    }

    /// Whether remote access is possible at all (a token is set).
    pub fn remote_enabled(&self) -> bool {
        self.token.borrow().is_some()
    }

    /// Replace the token. Remote sessions opened with the old one are closed.
    pub fn set(&self, token: Option<String>) {
        self.token
            .send_replace(token.filter(|t| !t.trim().is_empty()));
    }

    /// Follow token changes (a remote session watches this).
    pub fn subscribe(&self) -> watch::Receiver<Option<String>> {
        self.token.subscribe()
    }

    /// Whether `provided` is the token. Always false while no token is set,
    /// whatever is provided (an empty string included).
    pub fn allows(&self, provided: Option<&str>) -> bool {
        match (self.token.borrow().as_deref(), provided) {
            (Some(expected), Some(p)) => constant_time_eq(p.as_bytes(), expected.as_bytes()),
            _ => false,
        }
    }

    /// The link the game master opens on a phone (`<url>/control?g=…`), or
    /// `None` while remote access is disabled.
    pub fn share_url(&self, url: &str) -> Option<String> {
        self.current()
            .map(|t| format!("{}/control?{GM_PARAM}={t}", url.trim_end_matches('/')))
    }
}

/// How a request got access to the control panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GmAccess {
    /// From this computer, without a token.
    Local,
    /// From anywhere, with the game-master token.
    Token,
}

/// Why a request was turned away. Deliberately coarse: it never says whether a
/// token was wrong, missing or disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GmDenied {
    /// 401.
    Unauthorized,
    /// 429: too many failed attempts from this address.
    TooManyAttempts { retry_after_secs: u64 },
}

/// Remembers recent failed attempts per client address.
#[derive(Debug)]
pub struct AttemptLimiter {
    max_failures: usize,
    window: Duration,
    failures: HashMap<IpAddr, VecDeque<Instant>>,
}

/// Addresses tracked at once; past this the table is pruned (and, if need be,
/// reset) so an address-spraying client cannot grow it without bound.
const MAX_TRACKED_ADDRESSES: usize = 4096;

impl AttemptLimiter {
    pub fn new(max_failures: usize, window: Duration) -> Self {
        Self {
            max_failures,
            window,
            failures: HashMap::new(),
        }
    }

    /// One IPv6 /64 is one client (a host owns the whole prefix), IPv4-mapped
    /// IPv6 is the IPv4 address.
    fn key(ip: IpAddr) -> IpAddr {
        match ip.to_canonical() {
            IpAddr::V6(v6) => {
                let mut octets = v6.octets();
                octets[8..].fill(0);
                IpAddr::V6(octets.into())
            }
            v4 => v4,
        }
    }

    fn prune(queue: &mut VecDeque<Instant>, now: Instant, window: Duration) {
        while queue
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= window)
        {
            queue.pop_front();
        }
    }

    /// `Some(wait)` while `ip` is locked out.
    pub fn blocked_for(&mut self, ip: IpAddr, now: Instant) -> Option<Duration> {
        let window = self.window;
        let queue = self.failures.get_mut(&Self::key(ip))?;
        Self::prune(queue, now, window);
        if queue.len() < self.max_failures {
            return None;
        }
        // Locked until the oldest counted failure leaves the window.
        let oldest = *queue.front()?;
        Some((oldest + window).saturating_duration_since(now))
    }

    /// Count one failed attempt.
    pub fn record_failure(&mut self, ip: IpAddr, now: Instant) {
        let window = self.window;
        if self.failures.len() >= MAX_TRACKED_ADDRESSES {
            self.failures.retain(|_, q| {
                Self::prune(q, now, window);
                !q.is_empty()
            });
            if self.failures.len() >= MAX_TRACKED_ADDRESSES {
                self.failures.clear();
            }
        }
        let queue = self.failures.entry(Self::key(ip)).or_default();
        Self::prune(queue, now, window);
        // Never hold more than needed to decide.
        if queue.len() < self.max_failures {
            queue.push_back(now);
        }
    }
}

/// Decides whether a peer address is "this computer". Replaceable so tests can
/// pretend to be a remote client while connecting over loopback.
pub type LocalityFn = Arc<dyn Fn(&SocketAddr) -> bool + Send + Sync>;

/// The default locality test: the socket's peer is a loopback address.
pub fn is_loopback_peer(addr: &SocketAddr) -> bool {
    addr.ip().to_canonical().is_loopback()
}

/// Access rules of the control panel (`/control`, `/api/control/*`,
/// `/ws/control`): who may come in, and how often they may guess.
///
/// * loopback peers need no token (as before), unless the request was relayed
///   by a reverse proxy / tunnel running on this computer (`proxied`): then
///   the peer is the proxy, not the client, and a token is required;
/// * everyone else needs the game-master token;
/// * no token configured = no remote access;
/// * [`GM_MAX_FAILURES`] failures within [`GM_FAILURE_WINDOW`] lock the
///   address out; while locked even the right token is refused.
///
/// "Local" comes from the socket address only: `X-Forwarded-For` and friends
/// are never believed (they can only make a request *less* trusted).
#[derive(Clone)]
pub struct GmAuth {
    guard: GmGuard,
    limiter: Arc<Mutex<AttemptLimiter>>,
    is_local: LocalityFn,
}

impl std::fmt::Debug for GmAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GmAuth")
            .field("remote_enabled", &self.guard.remote_enabled())
            .finish_non_exhaustive()
    }
}

impl GmAuth {
    pub fn new(guard: GmGuard) -> Self {
        Self {
            guard,
            limiter: Arc::new(Mutex::new(AttemptLimiter::new(
                GM_MAX_FAILURES,
                GM_FAILURE_WINDOW,
            ))),
            is_local: Arc::new(is_loopback_peer),
        }
    }

    /// Replace the "is this peer local" test (tests use it to simulate a
    /// remote client over loopback).
    pub fn with_locality(mut self, is_local: LocalityFn) -> Self {
        self.is_local = is_local;
        self
    }

    /// Replace the limiter (tests use a small one).
    pub fn with_limits(mut self, max_failures: usize, window: Duration) -> Self {
        self.limiter = Arc::new(Mutex::new(AttemptLimiter::new(max_failures, window)));
        self
    }

    /// The token guard.
    pub fn guard(&self) -> &GmGuard {
        &self.guard
    }

    /// Decide one request. `bearer` / `query` are the two places a token may
    /// come from; `proxied` says the request carries reverse-proxy headers.
    pub fn authorize(
        &self,
        peer: Option<SocketAddr>,
        proxied: bool,
        bearer: Option<&str>,
        query: Option<&str>,
    ) -> Result<GmAccess, GmDenied> {
        self.authorize_at(peer, proxied, bearer, query, Instant::now())
    }

    /// [`authorize`](Self::authorize) with an explicit clock.
    pub fn authorize_at(
        &self,
        peer: Option<SocketAddr>,
        proxied: bool,
        bearer: Option<&str>,
        query: Option<&str>,
        now: Instant,
    ) -> Result<GmAccess, GmDenied> {
        if !proxied && peer.as_ref().is_some_and(|p| (self.is_local)(p)) {
            return Ok(GmAccess::Local);
        }
        // A peer-less request cannot be told apart from another one: they
        // share one bucket.
        let ip = peer
            .map(|p| p.ip())
            .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));

        let mut limiter = self.limiter.lock();
        if let Some(wait) = limiter.blocked_for(ip, now) {
            // Round up, never advertise 0.
            let retry_after_secs = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
            return Err(GmDenied::TooManyAttempts {
                retry_after_secs: retry_after_secs.max(1),
            });
        }
        // Both candidates are always compared (no short-circuit on the first).
        let by_header = self.guard.allows(bearer);
        let by_query = self.guard.allows(query);
        if by_header | by_query {
            Ok(GmAccess::Token)
        } else {
            limiter.record_failure(ip, now);
            Err(GmDenied::Unauthorized)
        }
    }
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

    #[test]
    fn constant_time_eq_handles_lengths() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abc"));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn gm_guard_without_a_token_never_matches() {
        let guard = GmGuard::default();
        assert!(!guard.allows(None));
        assert!(!guard.allows(Some("")));
        assert!(!guard.allows(Some("anything")));
        assert!(GmGuard::new(Some(String::new())).current().is_none());
        assert!(GmGuard::new(Some("  ".into())).current().is_none());
        assert!(!guard.remote_enabled());
        assert_eq!(guard.share_url("http://10.0.0.2:8080"), None);
    }

    #[test]
    fn gm_guard_needs_the_exact_token() {
        let guard = GmGuard::new(Some("tok123".into()));
        assert!(guard.allows(Some("tok123")));
        assert!(!guard.allows(Some("tok12")));
        assert!(!guard.allows(Some("tok1234")));
        assert!(!guard.allows(None));
        assert_eq!(
            guard.share_url("http://10.0.0.2:8080/").as_deref(),
            Some("http://10.0.0.2:8080/control?g=tok123")
        );
        let rx = guard.subscribe();
        guard.set(Some("new".into()));
        assert!(rx.has_changed().unwrap());
        assert!(!guard.allows(Some("tok123")));
    }

    fn peer(ip: &str) -> Option<SocketAddr> {
        Some(SocketAddr::new(ip.parse().unwrap(), 50_000))
    }

    #[test]
    fn auth_local_needs_no_token_remote_does() {
        let auth = GmAuth::new(GmGuard::new(Some("secret".into())));
        assert_eq!(
            auth.authorize(peer("127.0.0.1"), false, None, None),
            Ok(GmAccess::Local)
        );
        assert_eq!(
            auth.authorize(peer("::1"), false, None, None),
            Ok(GmAccess::Local)
        );
        assert_eq!(
            auth.authorize(peer("::ffff:127.0.0.1"), false, None, None),
            Ok(GmAccess::Local)
        );
        assert_eq!(
            auth.authorize(peer("192.168.1.9"), false, None, None),
            Err(GmDenied::Unauthorized)
        );
        assert_eq!(
            auth.authorize(peer("192.168.1.9"), false, Some("secret"), None),
            Ok(GmAccess::Token)
        );
        assert_eq!(
            auth.authorize(peer("192.168.1.9"), false, None, Some("secret")),
            Ok(GmAccess::Token)
        );
        // No peer address at all is never local.
        assert_eq!(
            auth.authorize(None, false, None, None),
            Err(GmDenied::Unauthorized)
        );
    }

    #[test]
    fn proxied_loopback_requests_are_treated_as_remote() {
        let auth = GmAuth::new(GmGuard::new(Some("secret".into())));
        assert_eq!(
            auth.authorize(peer("127.0.0.1"), true, None, None),
            Err(GmDenied::Unauthorized)
        );
        assert_eq!(
            auth.authorize(peer("127.0.0.1"), true, Some("secret"), None),
            Ok(GmAccess::Token)
        );
    }

    #[test]
    fn empty_token_forbids_remote_access_even_with_an_empty_credential() {
        let auth = GmAuth::new(GmGuard::new(Some(String::new())));
        for provided in [None, Some(""), Some("x")] {
            assert_eq!(
                auth.authorize(peer("10.1.1.1"), false, provided, provided),
                Err(GmDenied::Unauthorized)
            );
        }
        // The same answer as for a wrong token: nothing is revealed.
        let keyed = GmAuth::new(GmGuard::new(Some("secret".into())));
        assert_eq!(
            keyed.authorize(peer("10.1.1.1"), false, Some("nope"), None),
            Err(GmDenied::Unauthorized)
        );
        assert_eq!(
            auth.authorize(peer("127.0.0.1"), false, None, None),
            Ok(GmAccess::Local)
        );
    }

    #[test]
    fn limiter_locks_out_after_repeated_failures_and_recovers() {
        let auth = GmAuth::new(GmGuard::new(Some("secret".into())))
            .with_limits(3, Duration::from_secs(60));
        let t0 = Instant::now();
        let ip = peer("203.0.113.7");
        for _ in 0..3 {
            assert_eq!(
                auth.authorize_at(ip, false, Some("bad"), None, t0),
                Err(GmDenied::Unauthorized)
            );
        }
        // Locked: even the right token is refused.
        match auth.authorize_at(ip, false, Some("secret"), None, t0 + Duration::from_secs(1)) {
            Err(GmDenied::TooManyAttempts { retry_after_secs }) => {
                assert!((1..=60).contains(&retry_after_secs), "{retry_after_secs}");
            }
            other => panic!("expected a lockout, got {other:?}"),
        }
        // Other addresses are unaffected.
        assert_eq!(
            auth.authorize_at(peer("203.0.113.8"), false, Some("secret"), None, t0),
            Ok(GmAccess::Token)
        );
        // The lockout expires with the window.
        assert_eq!(
            auth.authorize_at(
                ip,
                false,
                Some("secret"),
                None,
                t0 + Duration::from_secs(61)
            ),
            Ok(GmAccess::Token)
        );
    }

    #[test]
    fn limiter_groups_ipv6_by_prefix_and_bounds_its_memory() {
        let mut limiter = AttemptLimiter::new(2, Duration::from_secs(60));
        let now = Instant::now();
        limiter.record_failure("2001:db8::1".parse().unwrap(), now);
        limiter.record_failure("2001:db8::2".parse().unwrap(), now);
        assert!(limiter
            .blocked_for("2001:db8::ffff".parse().unwrap(), now)
            .is_some());
        assert!(limiter
            .blocked_for("2001:db9::1".parse().unwrap(), now)
            .is_none());

        let mut limiter = AttemptLimiter::new(2, Duration::from_secs(60));
        for i in 0..(MAX_TRACKED_ADDRESSES as u32 * 2) {
            limiter.record_failure(IpAddr::V4(i.into()), now);
        }
        assert!(limiter.failures.len() <= MAX_TRACKED_ADDRESSES);
    }
}

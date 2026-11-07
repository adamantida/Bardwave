//! Choosing the address the console advertises to listeners.
//!
//! The old probe asked the OS which source address it would use to reach the
//! DNS server 8.8.8.8 and advertised the answer. That is correct on a plain home
//! network and wrong as soon as a proxy/VPN installs a TUN adapter: with Clash,
//! xray or sing-box in TUN mode the default route points into that adapter, the
//! probe answers with the adapter's own address out of the benchmarking range
//! (198.18.0.0/15), and the console published http://198.18.0.1:8080 -- an
//! address no player on the LAN can open, and one the operator cannot open
//! either.
//!
//! So the address is no longer taken from the route alone: every address the
//! machine actually owns is collected (adapter table plus the route probe) and
//! scored. Private LAN addresses win, global unicast comes next, and ranges that
//! cannot be reached from another machine -- benchmarking, link-local, loopback,
//! reserved -- never win at all. When nothing qualifies the caller falls back to
//! loopback, which is at least true.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};

/// Whether an address is one another machine on the network could open.
pub fn is_advertisable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_unspecified()
                || v4.is_loopback()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || v4.is_link_local()
                || is_benchmarking(v4)
                || v4.octets()[0] == 0
                || v4.octets()[0] >= 240)
        }
        IpAddr::V6(v6) => {
            !(v6.is_unspecified()
                || v6.is_loopback()
                || v6.is_multicast()
                || is_link_local_v6(v6)
                || is_documentation_v6(v6))
        }
    }
}

/// 198.18.0.0/15, the benchmarking range proxy TUN adapters hand out.
fn is_benchmarking(v4: Ipv4Addr) -> bool {
    let [a, b, ..] = v4.octets();
    a == 198 && (b & 0xfe) == 18
}

/// 100.64.0.0/10, carrier-grade NAT -- Tailscale and friends.
fn is_shared(v4: Ipv4Addr) -> bool {
    let [a, b, ..] = v4.octets();
    a == 100 && (64..=127).contains(&b)
}

/// fe80::/10.
fn is_link_local_v6(v6: Ipv6Addr) -> bool {
    (v6.segments()[0] & 0xffc0) == 0xfe80
}

/// fc00::/7.
fn is_unique_local_v6(v6: Ipv6Addr) -> bool {
    (v6.segments()[0] & 0xfe00) == 0xfc00
}

/// 2001:db8::/32, reserved for documentation.
fn is_documentation_v6(v6: Ipv6Addr) -> bool {
    let [a, b, ..] = v6.segments();
    a == 0x2001 && b == 0x0db8
}

/// How good an address is to advertise; None means never.
///
/// Higher wins. The absolute numbers carry no meaning beyond their order:
///
/// * RFC1918 beats global unicast, because the players are on the same LAN;
/// * inside RFC1918, 192.168.x.x beats 10.x.x.x beats 172.16/12 -- the last one
///   is where Docker, WSL and several TUN adapters put their virtual switches,
///   and those addresses are not the LAN;
/// * IPv6 unique-local and global unicast rank below a real IPv4 LAN, since an
///   unspecified bind is IPv4 by default;
/// * carrier-grade NAT ranks just above unusable, so a Tailscale address is
///   only ever a last resort before loopback.
fn score(ip: IpAddr) -> Option<u32> {
    if !is_advertisable(ip) {
        return None;
    }

    Some(match ip {
        IpAddr::V4(v4) => {
            if v4.is_private() {
                let [a, ..] = v4.octets();
                let preference = match a {
                    192 => 30,
                    10 => 20,
                    _ => 10, // 172.16.0.0/12
                };
                300 + preference
            } else if is_shared(v4) {
                160
            } else {
                200
            }
        }
        IpAddr::V6(v6) => {
            if is_unique_local_v6(v6) {
                130
            } else {
                120
            }
        }
    })
}

/// Every address this machine could be reached on, best guess first.
///
/// Only advertisable addresses are listed: an address that cannot be opened
/// from another machine is not a candidate for the published URL, and keeping
/// them out here means no caller can accidentally pick one.
///
/// The route probe is listed first so that, between two equally scored
/// addresses, the interface the default route actually uses wins.
pub fn candidates() -> Vec<IpAddr> {
    let mut found: Vec<IpAddr> = Vec::new();

    if let Some(ip) = probe_route() {
        found.push(ip);
    }

    for ip in adapter_addresses() {
        if is_advertisable(ip) && !found.contains(&ip) {
            found.push(ip);
        }
    }

    found
}

/// The address to advertise for a listener bound to the given address.
///
/// A wildcard bind is family-specific -- 0.0.0.0 never serves IPv6 -- so only
/// addresses of that family are considered.
pub fn pick_advertised(candidates: &[IpAddr], bind_addr: IpAddr) -> Option<IpAddr> {
    let family_is_ipv4 = bind_addr.is_ipv4();

    candidates
        .iter()
        .copied()
        .filter(|ip| !bind_addr.is_unspecified() || ip.is_ipv4() == family_is_ipv4)
        .filter_map(|ip| score(ip).map(|s| (s, ip)))
        .fold(None, |best: Option<(u32, IpAddr)>, candidate| match best {
            Some((best_score, _)) if best_score >= candidate.0 => best,
            _ => Some(candidate),
        })
        .map(|(_, ip)| ip)
}

/// Detect the address to advertise for a listener bound to a given address.
///
/// The bind decides the family: a socket on 0.0.0.0 serves IPv4 only, so an
/// IPv6 address would produce a URL nothing is listening on.
///
/// None means "no address here is reachable from anywhere else", which the
/// caller turns into loopback rather than into an unreachable guess.
pub fn detect_for_bind(bind_addr: IpAddr) -> Option<IpAddr> {
    pick_advertised(&candidates(), bind_addr)
}

/// The source address the OS would use for a route that leaves this machine.
///
/// Nothing is sent: the socket is only connected, which is enough for the OS to
/// pick an interface. The answer is filtered like every other candidate -- a
/// proxy TUN adapter lands in the benchmarking range and is dropped here.
fn probe_route() -> Option<IpAddr> {
    for target in ["8.8.8.8:80", "1.1.1.1:80"] {
        if let Ok(socket) = UdpSocket::bind("0.0.0.0:0") {
            if socket.connect(target).is_ok() {
                if let Ok(SocketAddr::V4(local)) = socket.local_addr() {
                    let ip = IpAddr::V4(*local.ip());
                    if is_advertisable(ip) {
                        return Some(ip);
                    }
                }
            }
        }
    }

    if let Ok(socket) = UdpSocket::bind("[::]:0") {
        if socket.connect("[2001:4860:4860::8888]:80").is_ok() {
            if let Ok(SocketAddr::V6(local)) = socket.local_addr() {
                let ip = IpAddr::V6(*local.ip());
                if is_advertisable(ip) {
                    return Some(ip);
                }
            }
        }
    }

    None
}

/// Source address of the default IPv4 route, unfiltered.
///
/// Unlike [`probe_route`] this keeps VPN / TUN addresses: it answers "which
/// adapter does internet traffic leave through", which is what decides
/// whether replies to players outside the LAN go back through the router.
pub fn default_route_source() -> Option<std::net::Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    match socket.local_addr().ok()? {
        SocketAddr::V4(local) => Some(*local.ip()),
        SocketAddr::V6(_) => None,
    }
}

/// A warning when internet traffic does not leave through the LAN adapter.
///
/// With a full-tunnel VPN up, the router forwards a player's connection to
/// our LAN address, but the reply follows the default route into the tunnel
/// and never reaches the player. Port forwarding itself works; the VPN has to
/// be off (or exclude Bardwave) for internet players.
pub fn vpn_warning(lan: Option<IpAddr>) -> Option<String> {
    let IpAddr::V4(lan) = lan? else {
        return None;
    };
    let route = default_route_source()?;
    (route != lan && !route.is_loopback())
        .then(|| format!("VPN active ({route}): internet players can't connect until it is off"))
}

#[cfg(windows)]
fn adapter_addresses() -> Vec<IpAddr> {
    windows::unicast_addresses()
}

/// Outside Windows the adapter table is not read: the route probe and the range
/// filter still keep TUN addresses out of the published URL, which is what this
/// module exists for. Reading the table elsewhere would need libc getifaddrs,
/// and this app ships for Windows.
#[cfg(unix)]
fn adapter_addresses() -> Vec<IpAddr> {
    unix::interface_addresses()
}

#[cfg(not(any(windows, unix)))]
fn adapter_addresses() -> Vec<IpAddr> {
    Vec::new()
}

/// Interface addresses on Linux / macOS / BSD via `getifaddrs`.
///
/// Without it only the route probe was available there, which picks the VPN
/// or Docker adapter whenever that owns the default route; the adapter list
/// lets the scoring prefer the real LAN address, as on Windows.
#[cfg(unix)]
mod unix {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    pub fn interface_addresses() -> Vec<IpAddr> {
        let mut found = Vec::new();
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        // SAFETY: getifaddrs allocates a linked list into `head`, released
        // with freeifaddrs below; nothing is kept past that.
        if unsafe { libc::getifaddrs(&mut head) } != 0 {
            return found;
        }
        let mut cursor = head;
        while !cursor.is_null() {
            // SAFETY: `cursor` is a node of the list returned above.
            let ifa = unsafe { &*cursor };
            cursor = ifa.ifa_next;

            let flags = ifa.ifa_flags as libc::c_uint;
            let up = flags & (libc::IFF_UP as libc::c_uint) != 0;
            let loopback = flags & (libc::IFF_LOOPBACK as libc::c_uint) != 0;
            if !up || loopback || ifa.ifa_addr.is_null() {
                continue;
            }
            // SAFETY: ifa_addr is non-null and points at a sockaddr whose
            // family tells which concrete type it is.
            let family = i32::from(unsafe { (*ifa.ifa_addr).sa_family });
            if family == libc::AF_INET {
                // SAFETY: family AF_INET means a sockaddr_in.
                let sin = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
                found.push(IpAddr::V4(Ipv4Addr::from(u32::from_be(
                    sin.sin_addr.s_addr,
                ))));
            } else if family == libc::AF_INET6 {
                // SAFETY: family AF_INET6 means a sockaddr_in6.
                let sin6 = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in6) };
                found.push(IpAddr::V6(Ipv6Addr::from(sin6.sin6_addr.s6_addr)));
            }
        }
        // SAFETY: `head` came from getifaddrs and is freed exactly once.
        unsafe { libc::freeifaddrs(head) };
        found
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn lists_without_loopback() {
            for ip in super::interface_addresses() {
                assert!(!ip.is_loopback(), "{ip}");
            }
        }
    }
}

/// Minimal GetAdaptersAddresses binding.
///
/// Declared by hand instead of pulling in windows-sys for one function: only the
/// prefix of IP_ADAPTER_ADDRESSES is read, which is valid because a C struct's
/// prefix keeps the offsets of the whole thing.
#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::ptr;

    const AF_UNSPEC: u32 = 0;
    const AF_INET: u16 = 2;
    const AF_INET6: u16 = 23;

    const GAA_FLAG_SKIP_ANYCAST: u32 = 0x0000_0002;
    const GAA_FLAG_SKIP_MULTICAST: u32 = 0x0000_0004;
    const GAA_FLAG_SKIP_DNS_SERVER: u32 = 0x0000_0008;

    const NO_ERROR: u32 = 0;
    const ERROR_BUFFER_OVERFLOW: u32 = 111;

    const IF_TYPE_LOOPBACK: u32 = 24;
    const IF_TYPE_TUNNEL: u32 = 131;
    const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 0xffff;
    const IF_OPER_STATUS_UP: u32 = 1;

    #[repr(C)]
    struct Sockaddr {
        family: u16,
        _rest: [u8; 14],
    }

    #[repr(C)]
    struct SockaddrIn {
        family: u16,
        _port: u16,
        address: [u8; 4],
        _zero: [u8; 8],
    }

    #[repr(C)]
    struct SockaddrIn6 {
        family: u16,
        _port: u16,
        _flowinfo: u32,
        address: [u8; 16],
        _scope_id: u32,
    }

    #[derive(Clone, Copy)]
    #[repr(C)]
    struct SocketAddress {
        sockaddr: *const Sockaddr,
        length: i32,
    }

    #[repr(C)]
    struct UnicastAddress {
        _length: u32,
        _flags: u32,
        next: *const UnicastAddress,
        address: SocketAddress,
    }

    #[repr(C)]
    struct AdapterAddresses {
        _length: u32,
        _if_index: u32,
        next: *const AdapterAddresses,
        _adapter_name: *const i8,
        first_unicast: *const UnicastAddress,
        _first_anycast: *const c_void,
        _first_multicast: *const c_void,
        _first_dns: *const c_void,
        _dns_suffix: *const u16,
        _description: *const u16,
        _friendly_name: *const u16,
        _physical_address: [u8; 8],
        _physical_address_length: u32,
        _flags: u32,
        _mtu: u32,
        if_type: u32,
        oper_status: u32,
    }

    #[link(name = "iphlpapi")]
    extern "system" {
        fn GetAdaptersAddresses(
            family: u32,
            flags: u32,
            reserved: *mut c_void,
            addresses: *mut AdapterAddresses,
            size: *mut u32,
        ) -> u32;
    }

    /// Addresses of every adapter that is up and is neither loopback nor tunnel.
    ///
    /// Tunnel adapters are the whole point: the TUN device of a proxy/VPN holds
    /// the benchmarking address that used to end up in the published URL, and
    /// the adapter table is the only place that tells it apart from the real
    /// network card.
    pub fn unicast_addresses() -> Vec<IpAddr> {
        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;

        // The first call only asks for the size the table needs; it always
        // reports overflow instead of filling anything in.
        let mut size: u32 = 0;
        let probe = unsafe {
            GetAdaptersAddresses(
                AF_UNSPEC,
                flags,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut size,
            )
        };
        if probe != ERROR_BUFFER_OVERFLOW || size == 0 {
            return Vec::new();
        }

        // The table holds 64-bit pointers, so the buffer has to be pointer
        // aligned; a byte vector is only 1-aligned.
        let words = (size as usize).div_ceil(std::mem::size_of::<usize>());
        let mut buffer: Vec<usize> = vec![0; words];
        let mut size_after = size;

        let status = unsafe {
            GetAdaptersAddresses(
                AF_UNSPEC,
                flags,
                ptr::null_mut(),
                buffer.as_mut_ptr().cast::<AdapterAddresses>(),
                &mut size_after,
            )
        };
        if status != NO_ERROR {
            return Vec::new();
        }

        let mut found = Vec::new();
        let mut adapter = buffer.as_ptr().cast::<AdapterAddresses>();

        while !adapter.is_null() {
            // SAFETY: the buffer holds at least one adapter and every next
            // pointer the API returned points inside that same buffer.
            let current = unsafe { &*adapter };

            let skip = current.oper_status != IF_OPER_STATUS_UP
                || matches!(
                    current.if_type,
                    IF_TYPE_LOOPBACK | IF_TYPE_TUNNEL | IF_TYPE_SOFTWARE_LOOPBACK
                );

            if !skip {
                let mut unicast = current.first_unicast;
                while !unicast.is_null() {
                    let entry = unsafe { &*unicast };
                    // SAFETY: the entry belongs to the adapter table, which is
                    // alive for as long as the buffer is.
                    if let Some(ip) = unsafe { address_of(entry.address) } {
                        found.push(ip);
                    }
                    unicast = entry.next;
                }
            }

            adapter = current.next;
        }

        found
    }

    /// # Safety
    /// address.sockaddr must point at a live sockaddr of the stated length.
    unsafe fn address_of(address: SocketAddress) -> Option<IpAddr> {
        if address.sockaddr.is_null() {
            return None;
        }

        match (*address.sockaddr).family {
            AF_INET if address.length as usize >= std::mem::size_of::<SockaddrIn>() => {
                let v4 = &*address.sockaddr.cast::<SockaddrIn>();
                Some(IpAddr::V4(Ipv4Addr::from(v4.address)))
            }
            AF_INET6 if address.length as usize >= std::mem::size_of::<SockaddrIn6>() => {
                let v6 = &*address.sockaddr.cast::<SockaddrIn6>();
                Some(IpAddr::V6(Ipv6Addr::from(v6.address)))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    fn v6(text: &str) -> IpAddr {
        IpAddr::V6(text.parse().expect("valid IPv6 literal"))
    }

    /// The regression this module exists for: a proxy TUN adapter answers the
    /// route probe with 198.18.0.1, and that must never be advertised.
    #[test]
    fn benchmarking_range_is_never_advertised() {
        assert!(!is_advertisable(v4(198, 18, 0, 1)));
        assert!(!is_advertisable(v4(198, 19, 255, 254)));
        assert_eq!(score(v4(198, 18, 0, 1)), None);
        assert_eq!(
            pick_advertised(&[v4(198, 18, 0, 1)], IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
            None,
            "a TUN-only machine has nothing to advertise but loopback"
        );
    }

    #[test]
    fn unusable_ranges_are_rejected() {
        for ip in [
            v4(0, 0, 0, 0),
            v4(127, 0, 0, 1),
            v4(169, 254, 10, 20),
            v4(224, 0, 0, 1),
            v4(255, 255, 255, 255),
            v4(192, 0, 2, 5),
            v4(240, 0, 0, 1),
            v6("::1"),
            v6("::"),
            v6("fe80::1"),
            v6("2001:db8::1"),
        ] {
            assert!(!is_advertisable(ip), "{ip} must not be advertised");
        }
    }

    #[test]
    fn lan_addresses_beat_everything_else() {
        let lan = v4(192, 168, 1, 245);
        let ten = v4(10, 0, 0, 7);
        let dockerish = v4(172, 17, 0, 1);
        let global = v4(8, 8, 8, 8);
        let tailscale = v4(100, 101, 102, 103);
        let ula = v6("fd00::1");

        assert!(score(lan) > score(ten), "192.168 should win over 10/8");
        assert!(
            score(ten) > score(dockerish),
            "10/8 should win over 172.16/12"
        );
        assert!(
            score(dockerish) > score(global),
            "a LAN beats a public address"
        );
        assert!(score(global) > score(tailscale));
        assert!(score(tailscale) > score(ula));
    }

    #[test]
    fn private_beats_public_regardless_of_order() {
        assert_eq!(
            pick_advertised(
                &[v4(8, 8, 8, 8), v4(192, 168, 0, 5)],
                IpAddr::V4(Ipv4Addr::UNSPECIFIED)
            ),
            Some(v4(192, 168, 0, 5))
        );
    }

    /// Equal scores keep the order they arrived in, so the route probe -- listed
    /// first by candidates -- breaks ties in favour of the uplink interface.
    #[test]
    fn equal_scores_keep_the_first_candidate() {
        let first = v4(192, 168, 1, 2);
        let second = v4(192, 168, 1, 3);
        assert_eq!(
            pick_advertised(&[first, second], IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
            Some(first)
        );
    }

    #[test]
    fn wildcard_binds_only_consider_their_own_family() {
        let candidates = [v6("2a00:1450:4001:800::1"), v4(192, 168, 1, 2)];

        assert_eq!(
            pick_advertised(&candidates, IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
            Some(v4(192, 168, 1, 2)),
            "an IPv4 wildcard socket cannot serve an IPv6 address"
        );
        assert_eq!(
            pick_advertised(&candidates, v6("::")),
            Some(v6("2a00:1450:4001:800::1"))
        );
    }

    #[test]
    fn detection_stays_within_advertisable_addresses() {
        let machine = candidates();
        let unusable: Vec<IpAddr> = machine
            .iter()
            .copied()
            .filter(|ip| !is_advertisable(*ip))
            .collect();

        assert!(
            unusable.is_empty(),
            "candidates carried addresses the URL can never use: {unusable:?} (all: {machine:?})"
        );
    }

    /// Whatever the route probe answers with has to pass the same filter as
    /// every other candidate.
    #[test]
    fn route_probe_answer_is_filtered_too() {
        if let Some(ip) = probe_route() {
            assert!(is_advertisable(ip), "route probe offered {ip}");
        }
    }
}

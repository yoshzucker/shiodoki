//! The three questions the agent asks the OS, and the session it runs in.
//!
//! Each OS answers in its own module.  Elsewhere the answers are fixed:
//! never locked, no Wi-Fi network, a session per boot of the agent.

use crate::watch::NetState;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{is_locked, session_id, ssid};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::{is_locked, session_id, ssid};

#[cfg(not(any(target_os = "macos", windows)))]
pub fn is_locked() -> bool {
    false
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn session_id() -> String {
    format!("agent-{}", std::process::id())
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn ssid() -> Option<String> {
    None
}

/// The network now: the Wi-Fi network and the interfaces that are up.
pub fn net_state() -> NetState {
    let mut links: Vec<String> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.ip() {
            std::net::IpAddr::V4(a) if !a.is_link_local() => Some(format!("{} {a}", i.name)),
            std::net::IpAddr::V6(a) if (a.segments()[0] & 0xffc0) != 0xfe80 => {
                Some(format!("{} v6", i.name))
            }
            _ => None,
        })
        .collect();
    links.sort();
    links.dedup();
    NetState {
        ssid: ssid(),
        links,
    }
}

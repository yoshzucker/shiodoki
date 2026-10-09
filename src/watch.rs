//! Turning what the agent samples every couple of seconds -- the lock, the
//! clocks, the network -- into the events the engine hears.
//!
//! Sampling rather than subscribing keeps what each OS has to provide down
//! to three questions (is the screen locked, which Wi-Fi network is this,
//! which addresses are up) and puts everything that decides on the answers
//! here, where it is the same on both and can be tested.

use std::time::Duration;

use jiff::Timestamp;

/// The network as far as shiodoki cares: the Wi-Fi network, and which
/// interfaces are up with which IPv4 addresses.  IPv6 counts by interface
/// alone, since temporary addresses come and go on their own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetState {
    pub ssid: Option<String>,
    pub links: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Locked,
    /// `since` is when the session was last in use -- unlocked and awake --
    /// before the lock, if the watch saw it.
    Unlocked {
        since: Option<Timestamp>,
    },
    /// `since` as for `Unlocked`, before the sleep.
    Woke {
        since: Option<Timestamp>,
    },
    Network {
        ssid: Option<String>,
    },
}

pub struct Sample {
    pub wall: Timestamp,
    /// Time since the agent started, by a clock that stops while the
    /// machine sleeps on macOS.
    pub mono: Duration,
    pub locked: bool,
    pub net: NetState,
}

/// How many samples in a row a new network state has to hold before it
/// counts: Wi-Fi coming back after a wake goes through several.
pub const SETTLE: u32 = 3;
/// By how many seconds the wall clock may run ahead of the monotonic one
/// before the difference can only be sleep.
const GAP_SECS: i64 = 30;
/// A wait between samples this long means the agent was not running, which
/// on a clock that does not stop in sleep is the only sign of one.
const STALL_SECS: i64 = 90;

#[derive(Default)]
pub struct Watch {
    last: Option<(Timestamp, Duration)>,
    locked: Option<bool>,
    /// The last sample that found the session unlocked.
    present: Option<Timestamp>,
    net: Option<NetState>,
    pending: Option<(NetState, u32)>,
    /// Woken, and the network not settled since: what was known before the
    /// sleep may be somewhere else's.
    unsure: bool,
}

impl Watch {
    pub fn locked(&self) -> bool {
        self.locked.unwrap_or(true)
    }

    /// The Wi-Fi network the network settled on, or none while it is
    /// settling again after a wake.
    pub fn ssid(&self) -> Option<String> {
        if self.unsure {
            return None;
        }
        self.net.as_ref().and_then(|n| n.ssid.clone())
    }

    /// What changed since the last sample.  The first sample is where the
    /// watch starts from, and changes nothing.
    pub fn observe(&mut self, s: Sample) -> Vec<Change> {
        let mut out = vec![];
        if let Some((wall, mono)) = self.last {
            let walked = s.wall.as_second() - wall.as_second();
            let ticked = s.mono.saturating_sub(mono).as_secs() as i64;
            if walked - ticked > GAP_SECS || walked > STALL_SECS {
                out.push(Change::Woke {
                    since: self.present,
                });
                self.unsure = true;
                self.pending = None;
            }
        }
        self.last = Some((s.wall, s.mono));

        if let Some(was) = self.locked
            && was != s.locked
        {
            out.push(if s.locked {
                Change::Locked
            } else {
                Change::Unlocked {
                    since: self.present,
                }
            });
        }
        self.locked = Some(s.locked);
        if !s.locked {
            self.present = Some(s.wall);
        }

        // After a wake even the network it had settled on has to settle
        // again before it is known; only a different one is a change.
        let same = self.net.as_ref() == Some(&s.net);
        if self.net.is_none() {
            self.net = Some(s.net);
        } else if same && !self.unsure {
            self.pending = None;
        } else {
            let n = match &self.pending {
                Some((p, n)) if *p == s.net => n + 1,
                _ => 1,
            };
            if n >= SETTLE {
                if !same {
                    out.push(Change::Network {
                        ssid: s.net.ssid.clone(),
                    });
                    self.net = Some(s.net);
                }
                self.pending = None;
                self.unsure = false;
            } else {
                self.pending = Some((s.net, n));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Clock {
        wall: i64,
        mono: u64,
    }

    impl Clock {
        fn sample(&mut self, secs: u64, slept: i64, locked: bool, net: &NetState) -> Sample {
            self.mono += secs;
            self.wall += secs as i64 + slept;
            Sample {
                wall: Timestamp::from_second(self.wall).unwrap(),
                mono: Duration::from_secs(self.mono),
                locked,
                net: net.clone(),
            }
        }
    }

    fn net(ssid: Option<&str>, links: &[&str]) -> NetState {
        NetState {
            ssid: ssid.map(str::to_string),
            links: links.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn the_first_sample_is_only_a_start() {
        let mut w = Watch::default();
        let mut c = Clock {
            wall: 1_800_000_000,
            mono: 0,
        };
        assert!(w.locked(), "locked until known otherwise");
        assert!(
            w.observe(c.sample(0, 0, false, &net(Some("Home"), &["en0 10.0.0.2"])))
                .is_empty()
        );
        assert!(!w.locked());
        assert_eq!(w.ssid(), Some("Home".into()));
    }

    #[test]
    fn lock_and_unlock() {
        let mut w = Watch::default();
        let mut c = Clock {
            wall: 1_800_000_000,
            mono: 0,
        };
        let n = net(None, &[]);
        w.observe(c.sample(0, 0, false, &n));
        assert_eq!(w.observe(c.sample(2, 0, true, &n)), [Change::Locked]);
        assert!(w.observe(c.sample(2, 0, true, &n)).is_empty());
        assert_eq!(
            w.observe(c.sample(2, 0, false, &n)),
            [Change::Unlocked {
                since: Some(Timestamp::from_second(1_800_000_000).unwrap())
            }],
            "last in use at the sample before the lock"
        );
    }

    #[test]
    fn sleep_shows_as_a_gap_between_the_clocks() {
        let mut w = Watch::default();
        let mut c = Clock {
            wall: 1_800_000_000,
            mono: 0,
        };
        let n = net(None, &[]);
        w.observe(c.sample(0, 0, false, &n));
        let used = Some(Timestamp::from_second(1_800_000_000).unwrap());
        // Lid closed: locked just before sleeping, woken an hour later.
        assert_eq!(w.observe(c.sample(2, 0, true, &n)), [Change::Locked]);
        assert_eq!(
            w.observe(c.sample(2, 3600, true, &n)),
            [Change::Woke { since: used }]
        );
        assert_eq!(
            w.observe(c.sample(2, 0, false, &n)),
            [Change::Unlocked { since: used }]
        );
        // A clock that keeps counting through sleep shows it as a stall.
        let before = Some(Timestamp::from_second(c.wall).unwrap());
        assert_eq!(
            w.observe(c.sample(600, 0, false, &n)),
            [Change::Woke { since: before }]
        );
        // An ordinary late sample is neither.
        assert!(w.observe(c.sample(5, 0, false, &n)).is_empty());
    }

    #[test]
    fn a_network_change_counts_once_it_settles() {
        let mut w = Watch::default();
        let mut c = Clock {
            wall: 1_800_000_000,
            mono: 0,
        };
        let home = net(Some("Home"), &["en0 10.0.0.2"]);
        let none = net(None, &[]);
        let office = net(Some("Office"), &["en0 172.16.0.9"]);
        w.observe(c.sample(0, 0, false, &home));
        // Off Wi-Fi briefly, then onto another: only where it settles counts.
        assert!(w.observe(c.sample(2, 0, false, &none)).is_empty());
        assert!(w.observe(c.sample(2, 0, false, &office)).is_empty());
        assert!(w.observe(c.sample(2, 0, false, &office)).is_empty());
        assert_eq!(
            w.observe(c.sample(2, 0, false, &office)),
            [Change::Network {
                ssid: Some("Office".into())
            }]
        );
        assert!(w.observe(c.sample(2, 0, false, &office)).is_empty());
        // A blip that goes back to where it was is nothing.
        assert!(w.observe(c.sample(2, 0, false, &none)).is_empty());
        assert!(w.observe(c.sample(2, 0, false, &office)).is_empty());
        assert!(w.observe(c.sample(2, 0, false, &office)).is_empty());
        assert!(w.observe(c.sample(2, 0, false, &office)).is_empty());
        // A second interface on the same Wi-Fi is a change, with the same SSID.
        let tunnel = net(Some("Office"), &["en0 172.16.0.9", "utun4 10.8.0.2"]);
        for _ in 1..SETTLE {
            assert!(w.observe(c.sample(2, 0, false, &tunnel)).is_empty());
        }
        assert_eq!(
            w.observe(c.sample(2, 0, false, &tunnel)),
            [Change::Network {
                ssid: Some("Office".into())
            }]
        );
    }

    #[test]
    fn after_a_wake_the_network_is_unknown_until_it_settles_again() {
        let mut w = Watch::default();
        let mut c = Clock {
            wall: 1_800_000_000,
            mono: 0,
        };
        let home = net(Some("Home"), &["en0 10.0.0.2"]);
        let office = net(Some("Office"), &["en0 172.16.0.9"]);
        w.observe(c.sample(0, 0, true, &home));
        // Asleep at home, woken somewhere else: Home is no longer known,
        // whatever the first samples still say.
        assert!(matches!(
            w.observe(c.sample(2, 3600, true, &home))[..],
            [Change::Woke { .. }]
        ));
        assert_eq!(w.ssid(), None);
        assert!(w.observe(c.sample(2, 0, true, &office)).is_empty());
        assert!(w.observe(c.sample(2, 0, true, &office)).is_empty());
        assert_eq!(w.ssid(), None);
        assert_eq!(
            w.observe(c.sample(2, 0, true, &office)),
            [Change::Network {
                ssid: Some("Office".into())
            }]
        );
        assert_eq!(w.ssid(), Some("Office".into()));
        // Woken where it slept: known again once it holds, the wake's own
        // sample counting, and no change.
        w.observe(c.sample(2, 3600, true, &office));
        for _ in 2..SETTLE {
            assert!(w.observe(c.sample(2, 0, true, &office)).is_empty());
            assert_eq!(w.ssid(), None);
        }
        assert!(w.observe(c.sample(2, 0, true, &office)).is_empty());
        assert_eq!(w.ssid(), Some("Office".into()));
    }
}

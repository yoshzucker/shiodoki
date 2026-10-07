//! The decision: given the rules, what has happened and what time it is,
//! which of them run now.
//!
//! Nothing here touches the OS.  The agent turns what the OS tells it into
//! an [`Observation`], hands it to [`step`] together with the state from
//! last time, and runs whatever comes back.  That makes every rule of the
//! README a test away.

use std::collections::BTreeMap;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

use crate::config::{Command, Config, EventKind, Rule, RuleId};
use crate::overrides::Overrides;
use crate::period::Period;

/// What `step` reads but never changes.
pub struct Context<'a> {
    pub config: &'a Config,
    pub overrides: &'a Overrides,
    /// Until when everything is paused on this machine.
    pub pause: Option<Timestamp>,
    pub tz: &'a TimeZone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The agent started, in the session known by `session` (its logon
    /// time, as the OS reports it), on the Wi-Fi network `ssid`.  A session
    /// not seen before is a login, and starts with no network known, so the
    /// one already joined counts as joining it.
    Start {
        session: String,
        ssid: Option<String>,
    },
    Unlock,
    Wake,
    /// The network settled into a different state; `ssid` is the Wi-Fi
    /// network now joined, if any.
    Network {
        ssid: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    pub now: Timestamp,
    pub locked: bool,
    /// `None` when nothing happened but the clock.
    pub event: Option<Event>,
}

/// This machine's record: the session, and per rule what ran and what is
/// waiting.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The Wi-Fi network last reported in this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssid: Option<String>,
    #[serde(default)]
    pub rules: BTreeMap<RuleId, RuleState>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleState {
    /// The opening of the period the rule last ran in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ran: Option<Timestamp>,
    /// When it last ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ran_at: Option<Timestamp>,
    /// The period the rule is due in, waiting to be clear.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<Period>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub rule: RuleId,
    pub period: Period,
    pub command: Command,
}

/// What happened besides running something, for the log and `status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    /// Became due, and waits to be clear.
    Waiting { rule: RuleId, period: Period },
    /// Was due, and its period closed first.
    Lapsed { rule: RuleId, period: Period },
    /// Was due, and has been skipped since.
    Skipped { rule: RuleId, period: Period },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub launches: Vec<Launch>,
    pub notes: Vec<Note>,
}

pub fn step(ctx: &Context, state: &mut State, obs: &Observation) -> Outcome {
    let now = obs.now;
    let mut fired: Vec<EventKind> = vec![];
    // The network joined, when this event is a change of Wi-Fi network.
    let mut joined: Option<String> = None;
    match &obs.event {
        Some(Event::Start { session, ssid }) => {
            if state.session.as_deref() != Some(session.as_str()) {
                state.session = Some(session.clone());
                fired.push(EventKind::Login);
                joined = ssid.clone();
            }
            state.ssid = ssid.clone();
        }
        Some(Event::Unlock) => fired.push(EventKind::Unlock),
        Some(Event::Wake) => fired.push(EventKind::Wake),
        Some(Event::Network { ssid }) => {
            fired.push(EventKind::Network);
            if *ssid != state.ssid {
                joined = ssid.clone();
            }
            state.ssid = ssid.clone();
        }
        None => {}
    }

    state
        .rules
        .retain(|id, _| ctx.config.rules.contains_key(id));
    let mut out = Outcome::default();
    for (id, rule) in &ctx.config.rules {
        let rs = state.rules.entry(id.clone()).or_default();
        if !rule.enabled {
            rs.due = None;
            continue;
        }

        if let Some(p) = rs.due {
            if now >= p.end {
                out.notes.push(Note::Lapsed {
                    rule: id.clone(),
                    period: p,
                });
                rs.due = None;
            } else if ctx.overrides.skips(id, p.day) {
                out.notes.push(Note::Skipped {
                    rule: id.clone(),
                    period: p,
                });
                rs.due = None;
            }
        }

        let mut newly = false;
        if rs.due.is_none()
            && (rule.is_timed() || listens(rule, &fired, joined.as_deref()))
            && let Some(p) = rule.period_at(now, ctx.config.day_starts, ctx.tz)
        {
            let done = (rule.is_timed() || rule.once) && rs.ran == Some(p.start);
            if !done && !ctx.overrides.skips(id, p.day) {
                rs.due = Some(p);
                newly = true;
            }
        }

        if let Some(p) = rs.due {
            if is_clear(ctx, rule, obs) {
                out.launches.push(Launch {
                    rule: id.clone(),
                    period: p,
                    command: rule.command.clone(),
                });
                rs.ran = Some(p.start);
                rs.ran_at = Some(now);
                rs.due = None;
            } else if newly {
                out.notes.push(Note::Waiting {
                    rule: id.clone(),
                    period: p,
                });
            }
        }
    }
    out
}

/// Whether what happened is something `rule` listens for.  A rule with
/// `ssid` hears joining one of those networks, however it was learned; a
/// rule without hears every network event, and nothing else as one.
fn listens(rule: &Rule, fired: &[EventKind], joined: Option<&str>) -> bool {
    rule.on.iter().any(|k| match k {
        EventKind::Network if !rule.ssid.is_empty() => {
            joined.is_some_and(|s| rule.ssid.iter().any(|x| x == s))
        }
        k => fired.contains(k),
    })
}

fn is_clear(ctx: &Context, rule: &Rule, obs: &Observation) -> bool {
    !(rule.unlocked && obs.locked)
        && ctx.pause.is_none_or(|until| obs.now >= until)
        && !ctx.overrides.blocks(&rule.id, obs.now)
}

/// The next moment after `now` at which [`step`] could decide differently
/// without anything happening: a timed period opening, a due period
/// closing, a pause or a block beginning or ending.  The agent wakes then,
/// or after a minute, whichever is sooner.
pub fn next_change(ctx: &Context, state: &State, now: Timestamp) -> Option<Timestamp> {
    let mut times = vec![];
    for rule in ctx.config.rules.values().filter(|r| r.enabled) {
        if rule.is_timed()
            && let Some(p) = rule.next_period(now, ctx.config.day_starts, ctx.tz)
        {
            times.push(p.start);
        }
        if let Some(p) = state.rules.get(&rule.id).and_then(|r| r.due) {
            times.push(p.end);
        }
    }
    times.extend(ctx.pause);
    for b in &ctx.overrides.blocks {
        times.push(b.from);
        times.push(b.to);
    }
    times.into_iter().filter(|t| *t > now).min()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::local;
    use crate::config::Os;
    use crate::overrides::{Block, Skip};
    use jiff::civil::{Date, date};

    /// A configuration, a clock and a lock, stepped by hand.
    struct World {
        config: Config,
        overrides: Overrides,
        pause: Option<Timestamp>,
        state: State,
        locked: bool,
        tz: TimeZone,
        notes: Vec<Note>,
    }

    fn world(rules: &str) -> World {
        World {
            config: Config::parse(&format!("day_starts = \"04:00\"\n{rules}"), Os::MacOS)
                .unwrap_or_else(|e| panic!("{e}")),
            overrides: Overrides::default(),
            pause: None,
            state: State::default(),
            locked: false,
            tz: TimeZone::fixed(jiff::tz::offset(9)),
            notes: vec![],
        }
    }

    impl World {
        fn ts(&self, d: Date, h: i8, m: i8) -> Timestamp {
            local(d.at(h, m, 0, 0), &self.tz)
        }

        /// Step at `d h:m`; the IDs of the rules that ran.
        fn at(&mut self, d: Date, h: i8, m: i8, event: Option<Event>) -> Vec<String> {
            let obs = Observation {
                now: self.ts(d, h, m),
                locked: self.locked,
                event,
            };
            let ctx = Context {
                config: &self.config,
                overrides: &self.overrides,
                pause: self.pause,
                tz: &self.tz,
            };
            let out = step(&ctx, &mut self.state, &obs);
            self.notes.extend(out.notes);
            out.launches.into_iter().map(|l| l.rule).collect()
        }

        fn tick(&mut self, d: Date, h: i8, m: i8) -> Vec<String> {
            self.at(d, h, m, None)
        }

        fn lapsed(&self) -> Vec<&str> {
            self.notes
                .iter()
                .filter_map(|n| match n {
                    Note::Lapsed { rule, .. } => Some(rule.as_str()),
                    _ => None,
                })
                .collect()
        }
    }

    const REVIEW: &str = r#"
        [rule.review]
        every = "FREQ=WEEKLY;BYDAY=TU"
        at = "10:00"
        until = "20m"
        open = "https://meet.example.com/review"
    "#;
    const TUE: Date = date(2026, 10, 13);
    const WED: Date = date(2026, 10, 14);

    fn session(s: &str) -> Option<Event> {
        Some(Event::Start {
            session: s.into(),
            ssid: None,
        })
    }

    #[test]
    fn a_timed_rule_runs_once_when_its_period_opens() {
        let mut w = world(REVIEW);
        assert!(w.tick(TUE, 9, 59).is_empty());
        assert_eq!(w.tick(TUE, 10, 0), ["review"]);
        assert!(w.tick(TUE, 10, 1).is_empty());
        assert!(w.at(TUE, 10, 2, Some(Event::Unlock)).is_empty());
        assert!(w.tick(WED, 10, 0).is_empty());
    }

    #[test]
    fn lid_closed_on_the_way_to_the_meeting() {
        // Closed at 9:58, opened in the meeting room at 10:05.
        let mut w = world(REVIEW);
        w.locked = true;
        assert!(w.tick(TUE, 10, 0).is_empty());
        assert!(matches!(w.notes[..], [Note::Waiting { .. }]));
        assert!(w.at(TUE, 10, 5, Some(Event::Wake)).is_empty());
        w.locked = false;
        assert_eq!(w.at(TUE, 10, 5, Some(Event::Unlock)), ["review"]);
    }

    #[test]
    fn too_late_is_not_at_all() {
        let mut w = world(REVIEW);
        w.locked = true;
        w.tick(TUE, 10, 0);
        w.locked = false;
        assert!(w.at(TUE, 10, 25, Some(Event::Unlock)).is_empty());
        assert_eq!(w.lapsed(), ["review"]);
    }

    #[test]
    fn a_period_that_opened_while_off_is_due_on_return() {
        let mut w = world(REVIEW);
        assert_eq!(w.at(TUE, 10, 10, session("a")), ["review"]);
        // Started too late: nothing, and nothing to say about it either.
        let mut w = world(REVIEW);
        assert!(w.at(TUE, 10, 30, session("a")).is_empty());
        assert!(w.notes.is_empty());
    }

    #[test]
    fn a_pause_postpones() {
        let mut w = world(REVIEW);
        w.pause = Some(w.ts(TUE, 10, 10));
        assert!(w.tick(TUE, 10, 0).is_empty());
        assert_eq!(w.tick(TUE, 10, 10), ["review"]);

        let mut w = world(REVIEW);
        w.pause = Some(w.ts(TUE, 11, 0));
        assert!(w.tick(TUE, 10, 0).is_empty());
        assert!(w.tick(TUE, 11, 0).is_empty());
        assert_eq!(w.lapsed(), ["review"]);
    }

    #[test]
    fn a_block_postpones_the_rules_it_names() {
        let mut w = world(&format!(
            "{REVIEW}\n[rule.other]\nevery = \"FREQ=WEEKLY;BYDAY=TU\"\nat = \"10:00\"\nrun = [\"x\"]"
        ));
        w.overrides.blocks.push(Block {
            from: w.ts(TUE, 9, 0),
            to: w.ts(TUE, 10, 30),
            rules: vec!["review".into()],
        });
        assert_eq!(w.tick(TUE, 10, 0), ["other"]);
        assert!(w.tick(TUE, 10, 30).is_empty());
        assert_eq!(w.lapsed(), ["review"]);
    }

    #[test]
    fn a_skip_cancels_even_what_is_waiting() {
        let mut w = world(REVIEW);
        w.overrides.skips.push(Skip {
            rule: "review".into(),
            from: TUE,
            to: TUE,
        });
        assert!(w.tick(TUE, 10, 0).is_empty());
        assert!(w.notes.is_empty());

        let mut w = world(REVIEW);
        w.locked = true;
        w.tick(TUE, 10, 0);
        w.overrides.skips.push(Skip {
            rule: "review".into(),
            from: TUE,
            to: TUE,
        });
        w.locked = false;
        assert!(w.at(TUE, 10, 5, Some(Event::Unlock)).is_empty());
        assert!(matches!(w.notes.last(), Some(Note::Skipped { .. })));
        // Taking the skip back while the period is open makes it due again.
        w.overrides.skips.clear();
        assert_eq!(w.tick(TUE, 10, 6), ["review"]);
    }

    #[test]
    fn login_is_a_session_not_a_start() {
        let mut w = world("[rule.mount]\non = [\"login\"]\nrun = [\"mount\"]");
        assert_eq!(w.at(TUE, 9, 0, session("monday-0900")), ["mount"]);
        assert!(
            w.at(TUE, 9, 30, session("monday-0900")).is_empty(),
            "a restart is not a login"
        );
        assert_eq!(w.at(TUE, 18, 0, session("monday-1800")), ["mount"]);
        assert!(w.at(TUE, 18, 5, Some(Event::Unlock)).is_empty());
    }

    #[test]
    fn once_a_morning_however_you_arrive() {
        let mut w = world(
            r#"
            [rule.morning]
            on = ["login", "unlock", "wake"]
            once = true
            every = "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"
            until = "12:00"
            run = ["capture"]
            "#,
        );
        assert_eq!(w.at(TUE, 8, 50, Some(Event::Unlock)), ["morning"]);
        assert!(w.at(TUE, 9, 30, Some(Event::Unlock)).is_empty());
        assert!(w.at(TUE, 9, 31, Some(Event::Wake)).is_empty());
        // Past midnight but before the day starts: still Tuesday's, done.
        assert!(w.at(WED, 2, 0, Some(Event::Unlock)).is_empty());
        assert!(
            w.at(WED, 12, 30, Some(Event::Unlock)).is_empty(),
            "after until"
        );
        assert!(
            w.at(date(2026, 10, 17), 9, 0, Some(Event::Unlock))
                .is_empty(),
            "Saturday"
        );
        assert_eq!(
            w.at(date(2026, 10, 15), 9, 0, Some(Event::Wake)),
            ["morning"]
        );
    }

    #[test]
    fn without_once_every_event_runs() {
        let mut w = world("[rule.u]\non = [\"unlock\"]\nrun = [\"x\"]");
        assert_eq!(w.at(TUE, 9, 0, Some(Event::Unlock)), ["u"]);
        assert_eq!(w.at(TUE, 9, 30, Some(Event::Unlock)), ["u"]);
        assert!(w.tick(TUE, 9, 31).is_empty());
    }

    #[test]
    fn an_event_behind_the_lock_screen_waits_for_it() {
        let mut w = world("[rule.w]\non = [\"wake\"]\nrun = [\"x\"]");
        w.locked = true;
        assert!(w.at(TUE, 9, 0, Some(Event::Wake)).is_empty());
        assert!(w.at(TUE, 9, 0, Some(Event::Wake)).is_empty());
        w.locked = false;
        // Two wakes held, one run.
        assert_eq!(w.at(TUE, 9, 1, Some(Event::Unlock)), ["w"]);
        assert!(w.tick(TUE, 9, 2).is_empty());
    }

    #[test]
    fn joining_a_network() {
        let mut w = world(
            r#"
            [rule.share]
            on = ["network"]
            ssid = ["Office"]
            unlocked = false
            run = ["mount"]
            [rule.any]
            on = ["network"]
            run = ["x"]
            "#,
        );
        let net = |s: Option<&str>| {
            Some(Event::Network {
                ssid: s.map(str::to_string),
            })
        };
        w.locked = true;
        w.at(TUE, 8, 0, session("s1"));
        assert_eq!(
            w.at(TUE, 8, 1, net(Some("Office"))),
            ["share"],
            "behind the lock screen, and only share"
        );
        w.locked = false;
        assert_eq!(
            w.at(TUE, 8, 30, net(Some("Office"))),
            ["any"],
            "another adapter coming up is not joining again"
        );
        assert_eq!(w.at(TUE, 12, 0, net(None)), ["any"]);
        assert_eq!(w.at(TUE, 13, 0, net(Some("Cafe"))), ["any"]);
        let mut back = w.at(TUE, 14, 0, net(Some("Office")));
        back.sort();
        assert_eq!(back, ["any", "share"]);
        // A new session starts with no network known: being on it already
        // is joining it, for the rules that name it and only those.
        let start = |s: &str, ssid: &str| {
            Some(Event::Start {
                session: s.into(),
                ssid: Some(ssid.into()),
            })
        };
        assert_eq!(w.at(WED, 8, 0, start("s2", "Office")), ["share"]);
        assert!(
            w.at(WED, 8, 5, start("s2", "Office")).is_empty(),
            "a restart is not joining"
        );
        assert!(
            w.at(WED, 8, 6, net(Some("Office")))
                .contains(&"any".to_string())
        );
        assert_eq!(w.at(WED, 9, 0, start("s3", "Cafe")), Vec::<String>::new());
    }

    #[test]
    fn disabled_and_removed_rules() {
        let mut w = world(&format!("{REVIEW}\nenabled = false"));
        assert!(w.tick(TUE, 10, 0).is_empty());

        let mut w = world(REVIEW);
        w.locked = true;
        w.tick(TUE, 10, 0);
        assert!(w.state.rules["review"].due.is_some());
        w.config.rules.clear();
        w.tick(TUE, 10, 1);
        assert!(w.state.rules.is_empty());
    }

    #[test]
    fn state_survives_a_round_trip_through_json() {
        let mut w = world(REVIEW);
        w.locked = true;
        w.at(TUE, 10, 0, session("s"));
        let text = serde_json::to_string_pretty(&w.state).unwrap();
        let back: State = serde_json::from_str(&text).unwrap();
        assert_eq!(back, w.state);
        assert_eq!(
            serde_json::from_str::<State>("{}").unwrap(),
            State::default()
        );
    }

    #[test]
    fn the_next_change() {
        let mut w = world(REVIEW);
        let next = |w: &World, d, h, m| {
            let ctx = Context {
                config: &w.config,
                overrides: &w.overrides,
                pause: w.pause,
                tz: &w.tz,
            };
            next_change(&ctx, &w.state, w.ts(d, h, m))
        };
        assert_eq!(next(&w, date(2026, 10, 12), 12, 0), Some(w.ts(TUE, 10, 0)));
        w.locked = true;
        w.tick(TUE, 10, 0);
        assert_eq!(
            next(&w, TUE, 10, 0),
            Some(w.ts(TUE, 10, 20)),
            "the waiting period closing"
        );
        w.pause = Some(w.ts(TUE, 10, 10));
        assert_eq!(next(&w, TUE, 10, 0), Some(w.ts(TUE, 10, 10)));
        w.overrides.blocks.push(Block {
            from: w.ts(TUE, 10, 5),
            to: w.ts(TUE, 10, 7),
            rules: vec![],
        });
        assert_eq!(next(&w, TUE, 10, 0), Some(w.ts(TUE, 10, 5)));
        assert_eq!(next(&w, TUE, 10, 5), Some(w.ts(TUE, 10, 7)));
    }
}

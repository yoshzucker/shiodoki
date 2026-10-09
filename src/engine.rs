//! The decision: given the rules, what has happened and what time it is,
//! which of them run now.
//!
//! Nothing here touches the OS.  The agent turns what the OS tells it into
//! an [`Observation`], hands it to [`step`] together with the state from
//! last time, and runs whatever comes back.  That makes every rule of the
//! README a test away.

use std::collections::{BTreeMap, BTreeSet};

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
    /// time, as the OS reports it).  A session not seen before is a login,
    /// and starts with no network known, so the one already joined counts
    /// as arriving on it.
    Start { session: String },
    /// The screen unlocked.  `since` is when the session was last in use
    /// before, if that is known.
    Unlock { since: Option<Timestamp> },
    /// The system resumed from sleep; `since` as for `Unlock`.
    Wake { since: Option<Timestamp> },
    /// The network settled into a different state.
    Network,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    pub now: Timestamp,
    pub locked: bool,
    /// The Wi-Fi network joined now, if one is and it is known.
    pub ssid: Option<String>,
    /// What the commands of `if` and `unless` answered lately: whether each
    /// succeeded.  One missing has no answer yet.
    pub answers: BTreeMap<Vec<String>, bool>,
    /// `None` when nothing happened but the clock.
    pub event: Option<Event>,
}

/// This machine's record: the session, the network, and per rule what ran
/// and what is waiting.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The Wi-Fi network last joined.  A stretch without Wi-Fi does not
    /// forget it: coming back onto it from none is not arriving.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssid: Option<String>,
    /// When it arrived on `ssid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrived: Option<Timestamp>,
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
    /// What it waits for, as of the last look.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<Hold>,
}

/// What keeps a due rule from being clear: the first of them, in the order
/// they are looked at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Hold {
    Locked,
    Paused,
    Blocked,
    /// Not on one of its networks.
    Away,
    If,
    Unless,
}

impl Hold {
    /// What `rule` waits for, as the log and `status` say it.
    pub fn describe(&self, rule: &Rule) -> String {
        match self {
            Hold::Locked => "for an unlock".into(),
            Hold::Paused => "for the pause to end".into(),
            Hold::Blocked => "for the block to end".into(),
            Hold::Away => format!("for {}", rule.ssid.join(" or ")),
            Hold::If => "for its `if` to succeed".into(),
            Hold::Unless => "for its `unless` to fail".into(),
        }
    }
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
    Waiting {
        rule: RuleId,
        period: Period,
        hold: Hold,
    },
    /// Was due, and its period closed first.
    Lapsed { rule: RuleId, period: Period },
    /// Was due, and has been skipped since.
    Skipped { rule: RuleId, period: Period },
    /// Joined a Wi-Fi network other than the one last joined.
    Arrived { ssid: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub launches: Vec<Launch>,
    pub notes: Vec<Note>,
    /// The commands of `if` and `unless` that due rules wait on and that
    /// have no answer: the agent runs them, and answers in a later
    /// observation.
    pub asks: BTreeSet<Vec<String>>,
}

pub fn step(ctx: &Context, state: &mut State, obs: &Observation) -> Outcome {
    let now = obs.now;
    let mut fired: Vec<EventKind> = vec![];
    match &obs.event {
        Some(Event::Start { session }) if state.session.as_deref() != Some(session.as_str()) => {
            state.session = Some(session.clone());
            fired.push(EventKind::Login);
            state.ssid = None;
            state.arrived = None;
        }
        Some(Event::Unlock { since }) => {
            fired.push(EventKind::Unlock);
            come_back(ctx, state, *since, now);
        }
        Some(Event::Wake { since }) => {
            fired.push(EventKind::Wake);
            come_back(ctx, state, *since, now);
        }
        Some(Event::Network) => fired.push(EventKind::Network),
        Some(Event::Start { .. }) | None => {}
    }

    let mut out = Outcome::default();
    // Arriving: being on a network other than the one last joined.
    let mut arrived: Option<&str> = None;
    if let Some(ssid) = &obs.ssid
        && state.ssid.as_ref() != Some(ssid)
    {
        state.ssid = Some(ssid.clone());
        state.arrived = Some(now);
        arrived = Some(ssid);
        out.notes.push(Note::Arrived { ssid: ssid.clone() });
    }

    state
        .rules
        .retain(|id, _| ctx.config.rules.contains_key(id));
    for (id, rule) in &ctx.config.rules {
        let rs = state.rules.entry(id.clone()).or_default();
        if !rule.enabled {
            rs.due = None;
            rs.hold = None;
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
            && (rule.is_timed() || listens(rule, &fired, arrived))
            && let Some(p) = rule.period_at(now, ctx.config.day_starts, ctx.tz)
        {
            let done = (rule.is_timed() || rule.once) && rs.ran == Some(p.start);
            if !done && !ctx.overrides.skips(id, p.day) {
                rs.due = Some(p);
                newly = true;
            }
        }

        rs.hold = None;
        if let Some(p) = rs.due {
            match hold(ctx, rule, obs, &mut out.asks) {
                None => {
                    out.launches.push(Launch {
                        rule: id.clone(),
                        period: p,
                        command: rule.command.clone(),
                    });
                    rs.ran = Some(p.start);
                    rs.ran_at = Some(now);
                    rs.due = None;
                }
                Some(h) => {
                    if newly {
                        out.notes.push(Note::Waiting {
                            rule: id.clone(),
                            period: p,
                            hold: h,
                        });
                    }
                    rs.hold = Some(h);
                }
            }
        }
    }
    out
}

/// Coming back to the machine on a new day, after it was last in use on an
/// earlier one, it knows no network, as after a login: the one it is on
/// counts as arrived on.  Only a network arrived on before the absence is
/// forgotten, so that a wake and the unlock after it arrive once.
fn come_back(ctx: &Context, state: &mut State, since: Option<Timestamp>, now: Timestamp) {
    let ds = ctx.config.day_starts;
    if let Some(since) = since
        && ds.day_of(since, ctx.tz) < ds.day_of(now, ctx.tz)
        && state.arrived.is_none_or(|a| a <= since)
    {
        state.ssid = None;
        state.arrived = None;
    }
}

/// Whether what happened is something `rule` listens for.  A rule with
/// `ssid` hears `network` as arriving on one of those networks, however it
/// was learned; a rule without hears every network event.
fn listens(rule: &Rule, fired: &[EventKind], arrived: Option<&str>) -> bool {
    rule.on.iter().any(|k| match k {
        EventKind::Network if !rule.ssid.is_empty() => {
            arrived.is_some_and(|s| rule.ssid.iter().any(|x| x == s))
        }
        k => fired.contains(k),
    })
}

/// What keeps a due rule from running now, if anything.  The checks come
/// last, being the only thing that costs a process to learn; one without
/// an answer is asked for.
fn hold(
    ctx: &Context,
    rule: &Rule,
    obs: &Observation,
    asks: &mut BTreeSet<Vec<String>>,
) -> Option<Hold> {
    if rule.unlocked && obs.locked {
        return Some(Hold::Locked);
    }
    if ctx.pause.is_some_and(|until| obs.now < until) {
        return Some(Hold::Paused);
    }
    if ctx.overrides.blocks(&rule.id, obs.now) {
        return Some(Hold::Blocked);
    }
    if !rule.ssid.is_empty() && !obs.ssid.as_ref().is_some_and(|s| rule.ssid.contains(s)) {
        return Some(Hold::Away);
    }
    for c in &rule.checks {
        let hold = if c.unless { Hold::Unless } else { Hold::If };
        match obs.answers.get(&c.argv) {
            Some(&succeeded) if succeeded != c.unless => {}
            Some(_) => return Some(hold),
            None => {
                asks.insert(c.argv.clone());
                return Some(hold);
            }
        }
    }
    None
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

    /// A configuration, a clock, a lock, a network and the checks'
    /// answers, stepped by hand.
    struct World {
        config: Config,
        overrides: Overrides,
        pause: Option<Timestamp>,
        state: State,
        locked: bool,
        ssid: Option<String>,
        answers: BTreeMap<Vec<String>, bool>,
        asks: BTreeSet<Vec<String>>,
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
            ssid: None,
            answers: BTreeMap::new(),
            asks: BTreeSet::new(),
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
                ssid: self.ssid.clone(),
                answers: self.answers.clone(),
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
            self.asks = out.asks;
            out.launches.into_iter().map(|l| l.rule).collect()
        }

        /// On `ssid`, or on no Wi-Fi.
        fn on(&mut self, ssid: Option<&str>) {
            self.ssid = ssid.map(str::to_string);
        }

        fn arrivals(&self) -> Vec<&str> {
            self.notes
                .iter()
                .filter_map(|n| match n {
                    Note::Arrived { ssid } => Some(ssid.as_str()),
                    _ => None,
                })
                .collect()
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
        Some(Event::Start { session: s.into() })
    }

    const UNLOCK: Option<Event> = Some(Event::Unlock { since: None });
    const WAKE: Option<Event> = Some(Event::Wake { since: None });
    const NETWORK: Option<Event> = Some(Event::Network);

    #[test]
    fn a_timed_rule_runs_once_when_its_period_opens() {
        let mut w = world(REVIEW);
        assert!(w.tick(TUE, 9, 59).is_empty());
        assert_eq!(w.tick(TUE, 10, 0), ["review"]);
        assert!(w.tick(TUE, 10, 1).is_empty());
        assert!(w.at(TUE, 10, 2, UNLOCK).is_empty());
        assert!(w.tick(WED, 10, 0).is_empty());
    }

    #[test]
    fn lid_closed_on_the_way_to_the_meeting() {
        // Closed at 9:58, opened in the meeting room at 10:05.
        let mut w = world(REVIEW);
        w.locked = true;
        assert!(w.tick(TUE, 10, 0).is_empty());
        assert!(matches!(
            w.notes[..],
            [Note::Waiting {
                hold: Hold::Locked,
                ..
            }]
        ));
        assert_eq!(w.state.rules["review"].hold, Some(Hold::Locked));
        assert!(w.at(TUE, 10, 5, WAKE).is_empty());
        w.locked = false;
        assert_eq!(w.at(TUE, 10, 5, UNLOCK), ["review"]);
    }

    #[test]
    fn too_late_is_not_at_all() {
        let mut w = world(REVIEW);
        w.locked = true;
        w.tick(TUE, 10, 0);
        w.locked = false;
        assert!(w.at(TUE, 10, 25, UNLOCK).is_empty());
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
        assert!(w.at(TUE, 10, 5, UNLOCK).is_empty());
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
        assert!(w.at(TUE, 18, 5, UNLOCK).is_empty());
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
        assert_eq!(w.at(TUE, 8, 50, UNLOCK), ["morning"]);
        assert!(w.at(TUE, 9, 30, UNLOCK).is_empty());
        assert!(w.at(TUE, 9, 31, WAKE).is_empty());
        // Past midnight but before the day starts: still Tuesday's, done.
        assert!(w.at(WED, 2, 0, UNLOCK).is_empty());
        assert!(w.at(WED, 12, 30, UNLOCK).is_empty(), "after until");
        assert!(
            w.at(date(2026, 10, 17), 9, 0, UNLOCK).is_empty(),
            "Saturday"
        );
        assert_eq!(w.at(date(2026, 10, 15), 9, 0, WAKE), ["morning"]);
    }

    #[test]
    fn without_once_every_event_runs() {
        let mut w = world("[rule.u]\non = [\"unlock\"]\nrun = [\"x\"]");
        assert_eq!(w.at(TUE, 9, 0, UNLOCK), ["u"]);
        assert_eq!(w.at(TUE, 9, 30, UNLOCK), ["u"]);
        assert!(w.tick(TUE, 9, 31).is_empty());
    }

    #[test]
    fn an_event_behind_the_lock_screen_waits_for_it() {
        let mut w = world("[rule.w]\non = [\"wake\"]\nrun = [\"x\"]");
        w.locked = true;
        assert!(w.at(TUE, 9, 0, WAKE).is_empty());
        assert!(w.at(TUE, 9, 0, WAKE).is_empty());
        w.locked = false;
        // Two wakes held, one run.
        assert_eq!(w.at(TUE, 9, 1, UNLOCK), ["w"]);
        assert!(w.tick(TUE, 9, 2).is_empty());
    }

    #[test]
    fn arriving_is_coming_from_another_network() {
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
        w.locked = true;
        w.on(Some("Home"));
        assert!(w.at(TUE, 8, 0, session("s1")).is_empty());
        assert_eq!(w.arrivals(), ["Home"], "a login knows no network");
        w.on(Some("Office"));
        assert_eq!(
            w.at(TUE, 8, 1, NETWORK),
            ["share"],
            "behind the lock screen, and only share"
        );
        w.locked = false;
        assert_eq!(
            w.at(TUE, 8, 30, NETWORK),
            ["any"],
            "another adapter coming up is not arriving again"
        );
        // The Wi-Fi gone for a while -- a sleep, a reconnect -- and back:
        // still where it was, not arriving.
        w.on(None);
        assert_eq!(w.at(TUE, 12, 0, NETWORK), ["any"]);
        w.on(Some("Office"));
        assert_eq!(w.at(TUE, 12, 5, NETWORK), ["any"]);
        w.on(Some("Cafe"));
        assert_eq!(w.at(TUE, 13, 0, NETWORK), ["any"]);
        w.on(Some("Office"));
        assert_eq!(w.at(TUE, 14, 0, NETWORK), ["any", "share"]);
        // A new session starts with no network known: being on it already
        // is arriving on it, for the rules that name it and only those.
        assert_eq!(w.at(WED, 8, 0, session("s2")), ["share"]);
        assert!(
            w.at(WED, 8, 5, session("s2")).is_empty(),
            "a restart is not arriving"
        );
        w.on(Some("Cafe"));
        assert!(w.at(WED, 9, 0, session("s3")).is_empty());
        assert_eq!(w.state.ssid.as_deref(), Some("Cafe"));
        assert_eq!(w.state.arrived, Some(w.ts(WED, 9, 0)));
    }

    #[test]
    fn coming_back_on_a_new_day_is_arriving() {
        let mut w = world(
            r#"
            [rule.home]
            on = ["network"]
            ssid = ["Home"]
            run = ["connect"]
            "#,
        );
        w.on(Some("Home"));
        assert_eq!(w.at(TUE, 19, 0, session("s1")), ["home"]);
        // Asleep from 23:00 to the morning: the wake knows no network until
        // the agent has seen it settle, and then Home is arrived on.
        w.on(None);
        w.locked = true;
        let night = Some(w.ts(TUE, 23, 0));
        assert!(
            w.at(WED, 8, 0, Some(Event::Wake { since: night }))
                .is_empty()
        );
        w.on(Some("Home"));
        assert!(w.tick(WED, 8, 0).is_empty(), "behind the lock screen");
        w.locked = false;
        assert_eq!(
            w.at(WED, 8, 1, Some(Event::Unlock { since: night })),
            ["home"],
            "and the unlock after the wake is not a second arrival"
        );
        assert_eq!(w.arrivals(), ["Home", "Home"]);
        // A screen locked overnight, without sleep, is the same.
        w.locked = true;
        w.at(WED, 22, 0, None);
        w.locked = false;
        let evening = Some(w.ts(WED, 22, 0));
        assert_eq!(
            w.at(
                date(2026, 10, 15),
                7,
                30,
                Some(Event::Unlock { since: evening })
            ),
            ["home"]
        );
        // Away over lunch is not a new day, nor is a late night past
        // midnight that the day has not ended for yet.
        let lunch = Some(w.ts(date(2026, 10, 15), 12, 0));
        assert!(
            w.at(
                date(2026, 10, 15),
                13,
                0,
                Some(Event::Wake { since: lunch })
            )
            .is_empty()
        );
        let late = Some(w.ts(date(2026, 10, 15), 23, 50));
        assert!(
            w.at(
                date(2026, 10, 16),
                2,
                0,
                Some(Event::Unlock { since: late })
            )
            .is_empty()
        );
    }

    #[test]
    fn a_rule_with_ssid_runs_only_there() {
        let mut w = world(
            r#"
            [rule.reminder]
            at = "17:30"
            until = "20:00"
            ssid = ["Office"]
            run = ["remind"]
            [rule.mount]
            on = ["login"]
            ssid = ["Office"]
            run = ["mount"]
            "#,
        );
        w.on(Some("Home"));
        assert!(w.at(TUE, 9, 0, session("s1")).is_empty());
        assert_eq!(w.state.rules["mount"].hold, Some(Hold::Away));
        assert!(w.tick(TUE, 17, 30).is_empty());
        assert_eq!(w.state.rules["reminder"].hold, Some(Hold::Away));
        // Held, like an event behind a lock screen: run on getting there,
        // if the period is still open -- and not while the Wi-Fi is down.
        w.on(None);
        assert!(w.at(TUE, 18, 0, NETWORK).is_empty());
        w.on(Some("Office"));
        assert_eq!(w.at(TUE, 18, 1, NETWORK), ["mount", "reminder"]);
        assert_eq!(w.state.rules["reminder"].hold, None);
    }

    #[test]
    fn checks_hold_a_rule_until_they_answer_as_it_needs() {
        let mut w = world(
            r#"
            [rule.sync]
            on = ["unlock"]
            if = ["drive-mapped"]
            run = ["sync"]
            [rule.map]
            on = ["unlock"]
            unless = ["drive-mapped"]
            run = ["map-drive"]
            "#,
        );
        let mapped = vec!["drive-mapped".to_string()];
        w.locked = true;
        w.at(TUE, 9, 0, UNLOCK);
        assert!(w.asks.is_empty(), "nothing is asked while it is locked");
        w.locked = false;
        assert!(w.at(TUE, 9, 1, UNLOCK).is_empty());
        assert_eq!(w.asks, BTreeSet::from([mapped.clone()]), "one ask for both");
        assert_eq!(w.state.rules["sync"].hold, Some(Hold::If));
        assert_eq!(w.state.rules["map"].hold, Some(Hold::Unless));
        // An answer stands until the agent lets it go; while it does, it is
        // not asked for again.
        w.answers.insert(mapped.clone(), false);
        assert_eq!(w.tick(TUE, 9, 1), ["map"]);
        assert!(w.asks.is_empty());
        w.answers.clear();
        assert!(w.tick(TUE, 9, 2).is_empty());
        assert_eq!(w.asks, BTreeSet::from([mapped.clone()]));
        w.answers.insert(mapped.clone(), true);
        assert_eq!(w.tick(TUE, 9, 3), ["sync"]);
        // Both on one rule: both have to hold.
        let mut w =
            world("[rule.r]\non = [\"unlock\"]\nif = [\"a\"]\nunless = [\"b\"]\nrun = [\"x\"]");
        w.answers.insert(vec!["a".into()], true);
        assert!(w.at(TUE, 9, 0, UNLOCK).is_empty());
        assert_eq!(w.asks, BTreeSet::from([vec!["b".to_string()]]));
        assert!(matches!(
            w.notes[..],
            [Note::Waiting {
                hold: Hold::Unless,
                ..
            }]
        ));
        w.answers.insert(vec!["b".into()], true);
        assert!(w.tick(TUE, 9, 1).is_empty());
        w.answers.insert(vec!["b".into()], false);
        assert_eq!(w.tick(TUE, 9, 2), ["r"]);
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

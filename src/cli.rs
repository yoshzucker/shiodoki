//! The commands, apart from parsing their arguments.
//!
//! Each takes the files from [`Paths`], and the time and zone from the
//! caller, so the tests can fix both.  Each returns what to print, or what
//! went wrong.

use std::fmt::Write as _;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;

use crate::clock::{DayStart, next_reading, parse_length, parse_time};
use crate::config::{Command, Config, Os, Rule};
use crate::launch::{self, Output, Started};
use crate::overrides::{Block, Overrides};
use crate::period::Period;
use crate::store::{self, Paths};
use crate::when::{parse_day, parse_range};

pub struct Cli {
    pub paths: Paths,
    pub now: Timestamp,
    pub tz: TimeZone,
    pub os: Os,
}

pub enum PauseFor {
    Length(String),
    Until(String),
    Today,
}

/// Which periods of a rule `skip` and `unskip` mean.
pub enum Which {
    /// The period now open, if the rule has not run in it; else the next.
    Next,
    Days(Vec<String>),
    /// Every period from today up to and including this day.
    Until(String),
}

/// How long ago a heartbeat still means the agent is running.
const ALIVE_SECS: i64 = 150;

impl Cli {
    fn config(&self) -> Result<Config, String> {
        store::load_config(&self.paths, self.os).map_err(|e| e.to_string())
    }

    fn overrides(&self) -> Result<Overrides, String> {
        store::load_overrides(&self.paths, &self.tz)
    }

    fn save(&self, mut o: Overrides, ds: DayStart) -> Result<(), String> {
        o.prune(self.now, ds.day_of(self.now, &self.tz));
        store::save_overrides(&self.paths, &o, &self.tz)
    }

    fn today(&self, ds: DayStart) -> Date {
        ds.day_of(self.now, &self.tz)
    }

    fn time(&self, t: Timestamp) -> String {
        t.to_zoned(self.tz.clone())
            .strftime("%a %m-%d %H:%M")
            .to_string()
    }

    fn period(&self, p: &Period) -> String {
        p.display(&self.tz)
    }

    fn rule<'c>(&self, config: &'c Config, id: &str) -> Result<&'c Rule, String> {
        config.rules.get(id).ok_or_else(|| {
            let ids: Vec<&str> = config.rules.keys().map(String::as_str).collect();
            format!("no rule {id:?}; the rules are: {}", ids.join(", "))
        })
    }

    pub fn check(&self) -> Result<String, String> {
        let config = self.config()?;
        self.overrides()?;
        let mut out = format!(
            "{}: {} rules\n",
            self.paths.config.display(),
            config.rules.len()
        );
        let width = config.rules.keys().map(String::len).max().unwrap_or(0);
        for (id, rule) in &config.rules {
            let _ = writeln!(
                out,
                "  {id:width$}  {}",
                self.describe(rule, config.day_starts)
            );
        }
        Ok(out)
    }

    fn describe(&self, rule: &Rule, ds: DayStart) -> String {
        let mut s = String::new();
        if !rule.enabled {
            s.push_str("disabled; ");
        }
        if !rule.is_timed() {
            let on: Vec<String> = rule
                .on
                .iter()
                .map(|k| format!("{k:?}").to_lowercase())
                .collect();
            let _ = write!(s, "on {}", on.join(", "));
            if !rule.ssid.is_empty() {
                let _ = write!(s, " ({})", rule.ssid.join(", "));
            }
            if rule.once {
                s.push_str(", once a period");
            }
            s.push_str("; ");
        }
        match rule.period_at(self.now, ds, &self.tz) {
            Some(p) => {
                let now = if rule.is_timed() {
                    "open now"
                } else {
                    "listening now"
                };
                let _ = write!(s, "{now}, {}", self.period(&p));
            }
            None => match rule.next_period(self.now, ds, &self.tz) {
                Some(p) => {
                    let _ = write!(s, "next {}", self.period(&p));
                }
                None => s.push_str("no period ahead"),
            },
        }
        s
    }

    pub fn status(&self) -> Result<String, String> {
        let config = self.config()?;
        let overrides = self.overrides()?;
        let state = store::load_state(&self.paths)?;
        let ds = config.day_starts;
        let mut out = String::new();

        let agent = match store::read_heartbeat(&self.paths)? {
            Some(t) if self.now.as_second() - t.as_second() <= ALIVE_SECS => {
                format!("running (seen {})", self.time(t))
            }
            Some(t) => format!("not running (last seen {})", self.time(t)),
            None => "not running".to_string(),
        };
        let _ = writeln!(out, "agent    {agent}");
        let pause = match store::read_pause(&self.paths, self.now)? {
            Some(t) => format!("paused until {}", self.time(t)),
            None => "not paused".to_string(),
        };
        let _ = writeln!(out, "pause    {pause}");
        let network = match (&state.ssid, state.arrived) {
            (Some(s), Some(t)) => format!("{s}, arrived {}", self.time(t)),
            (Some(s), None) => s.clone(),
            (None, _) => "none known".to_string(),
        };
        let _ = writeln!(out, "network  {network}");

        let waiting: Vec<(String, Period)> = state
            .rules
            .iter()
            .filter_map(|(id, r)| {
                r.due
                    .filter(|p| p.contains(self.now))
                    .map(|p| (id.clone(), p))
            })
            .collect();
        let width = config.rules.keys().map(String::len).max().unwrap_or(0);
        section(
            &mut out,
            "waiting",
            waiting
                .iter()
                .map(|(id, p)| format!("{id:width$}  {}", self.period(p))),
        );

        let mut next: Vec<(Period, &str)> = config
            .rules
            .values()
            .filter(|r| r.enabled && r.is_timed())
            .flat_map(|r| {
                r.upcoming(self.now, 3, ds, &self.tz)
                    .into_iter()
                    .map(move |p| (p, r.id.as_str()))
            })
            .filter(|(p, _)| p.start > self.now)
            .collect();
        next.sort_by_key(|(p, id)| (p.start, *id));
        next.truncate(8);
        section(
            &mut out,
            "next",
            next.iter().map(|(p, id)| {
                let mut line = format!("{}  {id}", self.period(p));
                if overrides.skips(id, p.day) {
                    line.push_str("  (skipped)");
                } else if let Some(b) = overrides.blocks.iter().find(|b| {
                    b.from <= p.start
                        && p.start < b.to
                        && (b.rules.is_empty() || b.rules.iter().any(|r| r == id))
                }) {
                    let _ = write!(line, "  (blocked until {})", self.time(b.to));
                }
                line
            }),
        );

        section(
            &mut out,
            "skips",
            overrides.skips.iter().map(|s| {
                if s.from == s.to {
                    format!("{:width$}  {}", s.rule, s.from)
                } else {
                    format!("{:width$}  {} .. {}", s.rule, s.from, s.to)
                }
            }),
        );
        section(
            &mut out,
            "blocks",
            overrides.blocks.iter().map(|b| {
                let p = Period {
                    day: b.from.to_zoned(self.tz.clone()).date(),
                    start: b.from,
                    end: b.to,
                };
                let who = if b.rules.is_empty() {
                    "every rule".to_string()
                } else {
                    b.rules.join(", ")
                };
                format!("{}  {who}", self.period(&p))
            }),
        );
        let _ = writeln!(out, "config   {}", self.paths.config.display());
        let _ = writeln!(out, "         {}", self.paths.overrides().display());
        let _ = writeln!(out, "state    {}", self.paths.state_dir.display());
        let login = match crate::install::installed() {
            Some(p) => format!("starts the agent ({})", p.display()),
            None => "not installed (shiodoki install)".to_string(),
        };
        let _ = writeln!(out, "login    {login}");
        Ok(out)
    }

    pub fn pause(&self, how: PauseFor) -> Result<String, String> {
        // The configuration is read only for `day_starts`, so a broken one
        // does not stop a pause from being set.
        let ds = self.config().map(|c| c.day_starts).unwrap_or_default();
        let until = match how {
            PauseFor::Length(s) => self
                .now
                .checked_add(parse_length(&s)?)
                .map_err(|e| e.to_string())?,
            PauseFor::Until(s) => next_reading(self.now, parse_time(&s)?, &self.tz),
            PauseFor::Today => ds.end(self.today(ds), &self.tz),
        };
        store::write_pause(&self.paths, Some(until), &self.tz)?;
        Ok(format!("paused until {}\n", self.time(until)))
    }

    pub fn resume(&self) -> Result<String, String> {
        let was = store::read_pause(&self.paths, self.now)?;
        store::write_pause(&self.paths, None, &self.tz)?;
        Ok(if was.is_some() {
            "resumed\n"
        } else {
            "was not paused\n"
        }
        .to_string())
    }

    /// The days `which` names for `rule`, checked against its schedule.
    fn days(
        &self,
        config: &Config,
        rule: &Rule,
        which: &Which,
        skipped: bool,
    ) -> Result<(Date, Date), String> {
        let ds = config.day_starts;
        let today = self.today(ds);
        match which {
            Which::Next => {
                let overrides = self.overrides()?;
                let state = store::load_state(&self.paths)?;
                let ran = state.rules.get(&rule.id).and_then(|r| r.ran);
                let open = rule
                    .period_at(self.now, ds, &self.tz)
                    .filter(|p| ran != Some(p.start));
                let found = open
                    .into_iter()
                    .chain(rule.upcoming(self.now, 400, ds, &self.tz))
                    .find(|p| overrides.skips(&rule.id, p.day) == skipped)
                    .ok_or_else(|| {
                        format!(
                            "{} has no {}period ahead",
                            rule.id,
                            if skipped { "skipped " } else { "" }
                        )
                    })?;
                Ok((found.day, found.day))
            }
            Which::Until(s) => {
                let d = parse_day(s, today)?;
                if d < today {
                    return Err(format!("{d} is in the past"));
                }
                Ok((today, d))
            }
            Which::Days(_) => unreachable!("handled one day at a time"),
        }
    }

    pub fn skip(&self, id: &str, which: Which) -> Result<String, String> {
        let config = self.config()?;
        let rule = self.rule(&config, id)?;
        let ds = config.day_starts;
        let today = self.today(ds);
        let mut o = self.overrides()?;
        let mut said = String::new();
        match &which {
            Which::Days(list) => {
                for s in list {
                    let d = parse_day(s, today)?;
                    if d < today {
                        return Err(format!("{d} is in the past"));
                    }
                    if !rule.falls_on(d) {
                        return Err(format!("{id} has no period on {d}"));
                    }
                    o.add_skip(id, d, d);
                    let _ = writeln!(
                        said,
                        "skipping {id} {}",
                        self.period(&rule.period_on(d, ds, &self.tz))
                    );
                }
            }
            _ => {
                let (from, to) = self.days(&config, rule, &which, false)?;
                o.add_skip(id, from, to);
                if from == to {
                    let _ = writeln!(
                        said,
                        "skipping {id} {}",
                        self.period(&rule.period_on(from, ds, &self.tz))
                    );
                } else {
                    let _ = writeln!(said, "skipping {id} from {from} to {to}");
                }
            }
        }
        self.save(o, ds)?;
        Ok(said)
    }

    pub fn unskip(&self, id: &str, which: Option<Which>) -> Result<String, String> {
        let ds = self.config().map(|c| c.day_starts).unwrap_or_default();
        let today = self.today(ds);
        let mut o = self.overrides()?;
        let changed = match &which {
            None => o.remove_skip(id, Date::MIN, Date::MAX),
            Some(Which::Days(list)) => {
                let mut any = false;
                for s in list {
                    let d = parse_day(s, today)?;
                    any |= o.remove_skip(id, d, d);
                }
                any
            }
            Some(w) => {
                let config = self.config()?;
                let rule = self.rule(&config, id)?;
                let (from, to) = self.days(&config, rule, w, true)?;
                o.remove_skip(id, from, to)
            }
        };
        self.save(o, ds)?;
        Ok(if changed {
            format!("no longer skipping those of {id}\n")
        } else {
            format!("{id} had nothing skipped there\n")
        })
    }

    fn window(
        &self,
        ds: DayStart,
        when: &str,
        range: Option<&str>,
    ) -> Result<(Timestamp, Timestamp), String> {
        let day = parse_day(when, self.today(ds))?;
        Ok(match range {
            Some(r) => {
                let (a, b) = parse_range(r)?;
                let from = ds.at(day, a, &self.tz);
                (from, next_reading(from, b, &self.tz))
            }
            None => (ds.start(day, &self.tz), ds.end(day, &self.tz)),
        })
    }

    pub fn block(
        &self,
        when: &str,
        range: Option<&str>,
        rules: Vec<String>,
    ) -> Result<String, String> {
        let config = self.config()?;
        for r in &rules {
            self.rule(&config, r)?;
        }
        let ds = config.day_starts;
        let (from, to) = self.window(ds, when, range)?;
        if to <= self.now {
            return Err("that is already over".into());
        }
        let mut o = self.overrides()?;
        let who = if rules.is_empty() {
            "every rule".to_string()
        } else {
            rules.join(", ")
        };
        let p = Period {
            day: from.to_zoned(self.tz.clone()).date(),
            start: from,
            end: to,
        };
        o.blocks.push(Block { from, to, rules });
        o.blocks.sort_by_key(|b| b.from);
        self.save(o, ds)?;
        Ok(format!("blocking {who} {}\n", self.period(&p)))
    }

    pub fn unblock(
        &self,
        when: Option<&str>,
        range: Option<&str>,
        rules: Vec<String>,
        all: bool,
    ) -> Result<String, String> {
        let ds = self.config().map(|c| c.day_starts).unwrap_or_default();
        let window = match (when, all) {
            (Some(w), false) => Some(self.window(ds, w, range)?),
            (None, true) => None,
            (Some(_), true) => return Err("give a day or --all, not both".into()),
            (None, false) => return Err("say which: a day (and times), or --all".into()),
        };
        let mut o = self.overrides()?;
        let n = o.remove_blocks(window, &rules);
        self.save(o, ds)?;
        Ok(match n {
            0 => "no block there\n".to_string(),
            1 => "removed 1 block\n".to_string(),
            n => format!("removed {n} blocks\n"),
        })
    }

    fn agent_alive(&self) -> Result<bool, String> {
        Ok(store::read_heartbeat(&self.paths)?
            .is_some_and(|t| self.now.as_second() - t.as_second() <= ALIVE_SECS))
    }

    /// Hand the running agent an event, as if the OS had sent it.
    pub fn fire(&self, event: &str, ssid: Option<&str>) -> Result<String, String> {
        let text = match (event, ssid) {
            ("network", Some(s)) => format!("network {s}"),
            (_, Some(_)) => return Err("--ssid goes with network".into()),
            (e @ ("unlock" | "wake" | "login" | "network"), None) => e.to_string(),
            (e, None) => {
                return Err(format!(
                    "{e:?} is not an event (login, unlock, wake, network)"
                ));
            }
        };
        if !self.agent_alive()? {
            return Err("the agent is not running".into());
        }
        let name = format!("{}-{}.event", self.now.as_millisecond(), std::process::id());
        store::write_atomic(&self.paths.inbox().join(name), &format!("{text}\n"))?;
        Ok(format!("handed {text} to the agent\n"))
    }

    /// Write the templates where there are no files yet.
    pub fn init(&self) -> Result<String, String> {
        let mut out = String::new();
        for (path, written) in store::init(&self.paths, &self.tz)? {
            let verb = if written { "wrote" } else { "left " };
            let _ = writeln!(out, "{verb} {}", path.display());
        }
        Ok(out)
    }

    /// Ask the running agent to run a rule's command now, and report what
    /// it says in its log: the whole way from the agent to the command, in
    /// the environment the login item gives it.
    pub fn try_with_agent(&self, id: &str, wait: std::time::Duration) -> Result<String, String> {
        let config = self.config()?;
        self.rule(&config, id)?;
        if !self.agent_alive()? {
            return Err("the agent is not running".into());
        }
        let log = self.paths.log();
        let from = std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0) as usize;
        let token = format!("{}-{}", self.now.as_millisecond(), std::process::id());
        store::write_atomic(
            &self.paths.inbox().join(format!("{token}-try.event")),
            &format!("run {id} {token}\n"),
        )?;
        let mark = format!(" for try {token}");
        let deadline = std::time::Instant::now() + wait;
        let mut said: Vec<String> = vec![];
        while std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let text = std::fs::read(&log).unwrap_or_default();
            let new = String::from_utf8_lossy(text.get(from..).unwrap_or_default()).into_owned();
            said = new
                .lines()
                .filter(|l| l.contains(&mark))
                // Without the timestamp and the token: the run is ours.
                .map(|l| {
                    l.split_once(' ')
                        .map_or(l, |(_, rest)| rest)
                        .replace(&mark, "")
                })
                .collect();
            let ended = said.iter().any(|l| {
                l.contains("handed to the OS")
                    || l.contains(" exited with ")
                    || l.starts_with("cannot run")
                    || l.starts_with("could not run")
            });
            if ended {
                let out: String = said.iter().map(|l| format!("agent: {l}\n")).collect();
                let ok = said
                    .iter()
                    .any(|l| l.contains("handed to the OS") || l.ends_with("exit status: 0"));
                return if ok {
                    Ok(out)
                } else {
                    Err(out.trim_end().to_string())
                };
            }
        }
        let mut out: String = said.iter().map(|l| format!("agent: {l}\n")).collect();
        out.push_str(if said.is_empty() {
            "the agent did not take it up; is it the agent for this configuration?"
        } else {
            "the agent has not said how it ended"
        });
        Err(out)
    }

    /// Run a rule's command now, in the foreground, whatever its schedule.
    pub fn try_rule(&self, id: &str) -> Result<String, String> {
        let config = self.config()?;
        let rule = self.rule(&config, id)?;
        match launch::start(&rule.command, &config.env, &Output::Inherit)? {
            Started::HandedOff => Ok(format!("{id}: handed to the OS\n")),
            Started::Child(mut child) => {
                let status = child.wait().map_err(|e| e.to_string())?;
                let what = match &rule.command {
                    Command::Run { .. } => "exited",
                    Command::Open { .. } => "open exited",
                };
                let how = launch::ended(status);
                if status.success() {
                    Ok(format!("{id}: {what} with {how}\n"))
                } else {
                    Err(format!("{id}: {what} with {how}"))
                }
            }
        }
    }
}

fn section(out: &mut String, name: &str, lines: impl Iterator<Item = String>) {
    for (i, line) in lines.enumerate() {
        let label = if i == 0 { name } else { "" };
        let _ = writeln!(out, "{label:8} {line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::local;
    use crate::engine::{RuleState, State};
    use jiff::civil::date;

    const CONFIG: &str = r#"
        day_starts = "04:00"
        [rule.review]
        every = "FREQ=WEEKLY;BYDAY=TU"
        at = "10:00"
        until = "20m"
        open = "https://meet.example.com/review"
        [rule.retro]
        every = "FREQ=WEEKLY;BYDAY=TH"
        at = "15:00"
        until = "15:15"
        run = ["sh", "-c", "exit 0"]
        [rule.morning]
        on = ["unlock"]
        once = true
        until = "12:00"
        run = ["sh", "-c", "exit 4"]
    "#;

    struct T {
        _dir: tempfile::TempDir,
        cli: Cli,
    }

    /// A CLI on Tuesday 2026-10-13 at `h:m`, in a fixed +09:00 zone.
    fn at(h: i8, m: i8) -> T {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: dir.path().join("config.toml"),
            state_dir: dir.path().join("state"),
        };
        std::fs::write(&paths.config, CONFIG).unwrap();
        let tz = TimeZone::fixed(jiff::tz::offset(9));
        let now = local(date(2026, 10, 13).at(h, m, 0, 0), &tz);
        T {
            _dir: dir,
            cli: Cli {
                paths,
                now,
                tz,
                os: Os::MacOS,
            },
        }
    }

    fn overrides(t: &T) -> Overrides {
        t.cli.overrides().unwrap()
    }

    #[test]
    fn check_reports_rules_and_problems() {
        let t = at(9, 0);
        let out = t.cli.check().unwrap();
        assert!(out.contains("3 rules"), "{out}");
        assert!(out.contains("review   next Tue 10-13 10:00-10:20"), "{out}");
        assert!(
            out.contains("morning  on unlock, once a period; listening now, Tue 10-13 04:00-12:00"),
            "{out}"
        );
        std::fs::write(&t.cli.paths.config, "[rule.x]\nat = \"25:00\"").unwrap();
        let err = t.cli.check().unwrap_err();
        assert!(
            err.contains("rule.x.at") && err.contains("nothing to do"),
            "{err}"
        );
        std::fs::remove_file(&t.cli.paths.config).unwrap();
        assert!(
            t.cli
                .check()
                .unwrap_err()
                .starts_with("no configuration at")
        );
    }

    #[test]
    fn pause_and_resume() {
        let t = at(9, 0);
        assert_eq!(
            t.cli.pause(PauseFor::Length("2h".into())).unwrap(),
            "paused until Tue 10-13 11:00\n"
        );
        assert_eq!(
            t.cli.pause(PauseFor::Until("8:30".into())).unwrap(),
            "paused until Wed 10-14 08:30\n"
        );
        assert_eq!(
            t.cli.pause(PauseFor::Today).unwrap(),
            "paused until Wed 10-14 04:00\n"
        );
        assert!(
            t.cli
                .status()
                .unwrap()
                .contains("paused until Wed 10-14 04:00")
        );
        assert_eq!(t.cli.resume().unwrap(), "resumed\n");
        assert_eq!(t.cli.resume().unwrap(), "was not paused\n");
    }

    #[test]
    fn skip_next_is_the_open_period_until_it_has_run() {
        let t = at(10, 5);
        assert_eq!(
            t.cli.skip("review", Which::Next).unwrap(),
            "skipping review Tue 10-13 10:00-10:20\n"
        );
        // Once skipped, the next is next week's.
        assert_eq!(
            t.cli.skip("review", Which::Next).unwrap(),
            "skipping review Tue 10-20 10:00-10:20\n"
        );
        assert_eq!(overrides(&t).skips.len(), 2);

        let t = at(10, 5);
        let mut state = State::default();
        let ran = local(date(2026, 10, 13).at(10, 0, 0, 0), &t.cli.tz);
        state.rules.insert(
            "review".into(),
            RuleState {
                ran: Some(ran),
                ran_at: Some(ran),
                due: None,
            },
        );
        store::save_state(&t.cli.paths, &state).unwrap();
        assert_eq!(
            t.cli.skip("review", Which::Next).unwrap(),
            "skipping review Tue 10-20 10:00-10:20\n"
        );
    }

    #[test]
    fn skip_days_and_ranges_are_checked() {
        let t = at(9, 0);
        assert!(
            t.cli
                .skip("review", Which::Days(vec!["thu".into()]))
                .unwrap_err()
                .contains("no period on 2026-10-15")
        );
        assert!(
            t.cli
                .skip("review", Which::Days(vec!["2026-10-06".into()]))
                .unwrap_err()
                .contains("in the past")
        );
        assert!(
            t.cli
                .skip("nope", Which::Next)
                .unwrap_err()
                .contains("the rules are: morning, retro, review")
        );
        assert_eq!(
            t.cli
                .skip("retro", Which::Until("2026-10-31".into()))
                .unwrap(),
            "skipping retro from 2026-10-13 to 2026-10-31\n"
        );
        assert_eq!(
            t.cli
                .unskip("retro", Some(Which::Days(vec!["2026-10-22".into()])))
                .unwrap(),
            "no longer skipping those of retro\n"
        );
        assert_eq!(overrides(&t).skips.len(), 2);
        assert_eq!(
            t.cli.unskip("retro", Some(Which::Next)).unwrap(),
            "no longer skipping those of retro\n"
        );
        let left: Vec<(Date, Date)> = overrides(&t).skips.iter().map(|s| (s.from, s.to)).collect();
        assert_eq!(
            left,
            vec![
                (date(2026, 10, 13), date(2026, 10, 14)),
                (date(2026, 10, 16), date(2026, 10, 21)),
                (date(2026, 10, 23), date(2026, 10, 31)),
            ]
        );
        assert_eq!(
            t.cli.unskip("retro", None).unwrap(),
            "no longer skipping those of retro\n"
        );
        assert!(
            overrides(&t).skips.is_empty(),
            "nothing is skipped any more"
        );
    }

    #[test]
    fn block_and_unblock() {
        let t = at(9, 0);
        assert_eq!(
            t.cli.block("thu", Some("13:00-15:30"), vec![]).unwrap(),
            "blocking every rule Thu 10-15 13:00-15:30\n"
        );
        assert_eq!(
            t.cli.block("today", None, vec!["review".into()]).unwrap(),
            "blocking review Tue 10-13 04:00 - Wed 10-14 04:00\n"
        );
        assert!(
            t.cli
                .block("today", Some("06:00-07:00"), vec![])
                .unwrap_err()
                .contains("already over")
        );
        assert!(t.cli.block("today", None, vec!["nope".into()]).is_err());
        let status = t.cli.status().unwrap();
        assert!(
            status.contains("Tue 10-13 10:00-10:20  review  (blocked until Wed 10-14 04:00)"),
            "{status}"
        );
        assert!(
            status.contains("Thu 10-15 15:00-15:15  retro  (blocked until Thu 10-15 15:30)"),
            "{status}"
        );
        assert_eq!(
            t.cli
                .unblock(Some("thu"), Some("14:00-14:10"), vec![], false)
                .unwrap(),
            "removed 1 block\n"
        );
        assert_eq!(
            t.cli.unblock(None, None, vec![], false).unwrap_err(),
            "say which: a day (and times), or --all"
        );
        assert_eq!(
            t.cli.unblock(None, None, vec![], true).unwrap(),
            "removed 1 block\n"
        );
    }

    #[test]
    fn status_says_what_waits_and_what_comes() {
        let t = at(10, 5);
        let mut state = State::default();
        let p = Period {
            day: date(2026, 10, 13),
            start: local(date(2026, 10, 13).at(10, 0, 0, 0), &t.cli.tz),
            end: local(date(2026, 10, 13).at(10, 20, 0, 0), &t.cli.tz),
        };
        state.rules.insert(
            "review".into(),
            RuleState {
                ran: None,
                ran_at: None,
                due: Some(p),
            },
        );
        state.ssid = Some("Example-Office".into());
        state.arrived = Some(local(date(2026, 10, 13).at(9, 40, 0, 0), &t.cli.tz));
        store::save_state(&t.cli.paths, &state).unwrap();
        store::write_heartbeat(&t.cli.paths, t.cli.now).unwrap();
        t.cli.skip("retro", Which::Next).unwrap();
        let s = t.cli.status().unwrap();
        assert!(s.contains("agent    running (seen Tue 10-13 10:05)"), "{s}");
        assert!(s.contains("pause    not paused"), "{s}");
        assert!(
            s.contains("network  Example-Office, arrived Tue 10-13 09:40"),
            "{s}"
        );
        assert!(s.contains("waiting  review   Tue 10-13 10:00-10:20"), "{s}");
        assert!(
            s.contains("next     Thu 10-15 15:00-15:15  retro  (skipped)"),
            "{s}"
        );
        assert!(s.contains("skips    retro    2026-10-15"), "{s}");
    }

    #[test]
    fn try_with_the_agent_reads_its_answer_from_the_log() {
        let t = at(9, 0);
        let wait = std::time::Duration::from_millis(300);
        assert_eq!(
            t.cli.try_with_agent("retro", wait).unwrap_err(),
            "the agent is not running"
        );
        store::write_heartbeat(&t.cli.paths, t.cli.now).unwrap();
        let err = t.cli.try_with_agent("retro", wait).unwrap_err();
        assert!(err.contains("did not take it up"), "{err}");
        std::fs::remove_dir_all(t.cli.paths.inbox()).unwrap();

        // An agent that answers: a thread of this test, writing the log.
        let paths = t.cli.paths.clone();
        let answer = std::thread::spawn(move || {
            for _ in 0..100 {
                std::thread::sleep(std::time::Duration::from_millis(50));
                let found = std::fs::read_dir(paths.inbox())
                    .ok()
                    .and_then(|mut d| d.next())
                    .and_then(|e| e.ok())
                    .map(|e| e.path());
                let Some(f) = found else { continue };
                let asked = std::fs::read_to_string(&f).unwrap();
                let token = asked.split_whitespace().nth(2).unwrap().to_string();
                assert!(asked.starts_with("run retro "));
                std::fs::remove_file(&f).unwrap();
                let mut log = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(paths.log())
                    .unwrap();
                use std::io::Write as _;
                writeln!(log, "T ran retro for try {token}, pid 1").unwrap();
                writeln!(log, "T retro for try {token} exited with exit status: 0").unwrap();
                return;
            }
            panic!("nothing was asked");
        });
        let out = t
            .cli
            .try_with_agent("retro", std::time::Duration::from_secs(5))
            .unwrap();
        answer.join().unwrap();
        assert_eq!(
            out,
            "agent: ran retro, pid 1\nagent: retro exited with exit status: 0\n"
        );
    }

    #[test]
    fn init_reports_what_it_wrote() {
        let t = at(9, 0);
        assert_eq!(
            t.cli.init().unwrap(),
            format!(
                "left  {}\nwrote {}\n",
                t.cli.paths.config.display(),
                t.cli.paths.overrides().display()
            )
        );
    }

    #[test]
    fn fire_needs_a_running_agent() {
        let t = at(9, 0);
        assert_eq!(
            t.cli.fire("unlock", None).unwrap_err(),
            "the agent is not running"
        );
        store::write_heartbeat(&t.cli.paths, t.cli.now).unwrap();
        assert_eq!(
            t.cli.fire("network", Some("My Office")).unwrap(),
            "handed network My Office to the agent\n"
        );
        assert!(t.cli.fire("unlock", Some("x")).is_err());
        assert!(t.cli.fire("lunch", None).is_err());
        let files: Vec<_> = std::fs::read_dir(t.cli.paths.inbox()).unwrap().collect();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn try_runs_now_and_reports_the_exit() {
        let t = at(9, 0);
        assert_eq!(
            t.cli.try_rule("retro").unwrap(),
            "retro: exited with exit status: 0\n"
        );
        assert_eq!(
            t.cli.try_rule("morning").unwrap_err(),
            "morning: exited with exit status: 4"
        );
    }
}

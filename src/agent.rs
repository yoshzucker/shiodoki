//! The resident loop: sample, decide, run, remember.
//!
//! [`Agent`] is everything the agent does short of asking the OS and
//! drawing the icon: it is handed a [`Sample`] every couple of seconds and
//! answers with what to run.  The program in `src/bin` does the asking, the
//! running and the drawing.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

use crate::config::{Config, Os};
use crate::engine::{self, Context, Event, Launch, Note, Observation, State};
use crate::launch::{self, Output, Started};
use crate::overrides::Overrides;
use crate::store::{self, ConfigLoad, Paths};
use crate::watch::{Change, Sample, Watch};

/// How often the agent says it is alive.
const HEARTBEAT_SECS: i64 = 30;
/// Where a log that has grown past this is moved aside.
const LOG_LIMIT: u64 = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    Pause(SignedDuration),
    PauseToday,
    Resume,
    OpenConfig,
    Quit,
}

/// What the icon shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub headline: String,
    pub next: Option<String>,
    pub paused: bool,
    pub problem: bool,
}

/// A file's identity for noticing that it changed.
type Stamp = Option<(SystemTime, u64)>;

fn stamp(path: &Path) -> Stamp {
    fs::metadata(path)
        .ok()
        .and_then(|m| Some((m.modified().ok()?, m.len())))
}

pub struct Agent {
    paths: Paths,
    os: Os,
    tz: TimeZone,
    config: Option<Config>,
    config_stamp: Stamp,
    problem: Option<String>,
    overrides: Overrides,
    overrides_stamp: Stamp,
    pause: Option<Timestamp>,
    state: State,
    watch: Watch,
    last_beat: Option<Timestamp>,
    /// The start, kept for when there is a configuration to hear it.
    start: Option<Event>,
    /// Asked to stop, by `shiodoki uninstall`.
    quit: bool,
}

impl Agent {
    pub fn new(paths: Paths, os: Os, tz: TimeZone) -> Agent {
        let mut a = Agent {
            paths,
            os,
            tz,
            config: None,
            config_stamp: None,
            problem: None,
            overrides: Overrides::default(),
            overrides_stamp: None,
            pause: None,
            state: State::default(),
            watch: Watch::default(),
            last_beat: None,
            start: None,
            quit: false,
        };
        match store::load_state(&a.paths) {
            Ok(s) => a.state = s,
            Err(e) => a.log(&format!("starting from no state: {e}")),
        }
        a
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// Whether something asked the agent to stop.
    pub fn wants_quit(&self) -> bool {
        self.quit
    }

    /// The first sample, in the session `session`.
    pub fn begin(&mut self, s: Sample, session: String) -> Vec<Launch> {
        let now = s.wall;
        self.log(&format!(
            "started in {session} ({})",
            self.paths.config.display()
        ));
        self.reload(now);
        self.watch.observe(s);
        let ssid = self.watch.ssid();
        self.decide(now, vec![Some(Event::Start { session, ssid })])
    }

    pub fn tick(&mut self, s: Sample) -> Vec<Launch> {
        let now = s.wall;
        self.reload(now);
        let mut events = vec![];
        for c in self.watch.observe(s) {
            self.log(&format!("{c:?}").to_lowercase());
            events.push(match c {
                Change::Locked => None,
                Change::Unlocked => Some(Event::Unlock),
                Change::Woke => Some(Event::Wake),
                Change::Network { ssid } => Some(Event::Network { ssid }),
            });
        }
        events.extend(self.inbox().into_iter().map(Some));
        if events.is_empty() {
            events.push(None);
        }
        self.decide(now, events)
    }

    fn decide(&mut self, now: Timestamp, mut events: Vec<Option<Event>>) -> Vec<Launch> {
        let Some(config) = &self.config else {
            // Without rules nothing can be decided, but a login has to be
            // remembered until there are some: it will not come again.
            if let Some(start) = events
                .into_iter()
                .flatten()
                .find(|e| matches!(e, Event::Start { .. }))
            {
                self.start = Some(start);
            }
            self.beat(now);
            return vec![];
        };
        if let Some(start) = self.start.take() {
            events.insert(0, Some(start));
        }
        let ctx = Context {
            config,
            overrides: &self.overrides,
            pause: self.pause,
            tz: &self.tz,
        };
        let before = self.state.clone();
        let mut launches = vec![];
        let mut notes = vec![];
        for event in events {
            let obs = Observation {
                now,
                locked: self.watch.locked(),
                event,
            };
            let out = engine::step(&ctx, &mut self.state, &obs);
            launches.extend(out.launches);
            notes.extend(out.notes);
        }
        for n in notes {
            let line = match n {
                Note::Waiting { rule, .. } => format!("{rule} is due, and waits"),
                Note::Lapsed { rule, .. } => format!("{rule} lapsed: its period closed first"),
                Note::Skipped { rule, .. } => format!("{rule} was waiting, and is skipped"),
            };
            self.log(&line);
        }
        if self.state != before
            && let Err(e) = store::save_state(&self.paths, &self.state)
        {
            self.log(&e);
        }
        self.beat(now);
        launches
    }

    fn beat(&mut self, now: Timestamp) {
        if self
            .last_beat
            .is_none_or(|t| now.as_second() - t.as_second() >= HEARTBEAT_SECS)
        {
            if let Err(e) = store::write_heartbeat(&self.paths, now) {
                self.log(&e);
            }
            self.last_beat = Some(now);
        }
    }

    /// Read again what has changed: the configuration, the overrides and
    /// the pause.  A configuration with an error leaves the last good one in
    /// force.
    fn reload(&mut self, now: Timestamp) {
        let s = stamp(&self.paths.config);
        if s != self.config_stamp || (self.config.is_none() && self.problem.is_none()) {
            self.config_stamp = s;
            match store::load_config(&self.paths, self.os) {
                Ok(c) => {
                    if self.problem.take().is_some() || self.config.is_some() {
                        self.log("configuration read again");
                    }
                    self.config = Some(c);
                }
                Err(e) => {
                    let msg = match &e {
                        ConfigLoad::Missing(_) => e.to_string(),
                        _ => format!("{e}\n(the last good configuration stays in force)"),
                    };
                    if self.problem.as_deref() != Some(&msg) {
                        self.log(&msg);
                    }
                    self.problem = Some(msg);
                }
            }
        }
        let s = stamp(&self.paths.overrides());
        if s != self.overrides_stamp {
            self.overrides_stamp = s;
            match store::load_overrides(&self.paths, &self.tz) {
                Ok(o) => self.overrides = o,
                Err(e) => self.log(&format!("{e}\n(the last good overrides stay in force)")),
            }
        }
        match store::read_pause(&self.paths, now) {
            Ok(p) => self.pause = p,
            Err(e) => self.log(&e),
        }
    }

    /// Events handed over by `shiodoki fire`, oldest first.
    fn inbox(&mut self) -> Vec<Event> {
        let dir = self.paths.inbox();
        let Ok(entries) = fs::read_dir(&dir) else {
            return vec![];
        };
        let mut files: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        files.sort();
        let mut out = vec![];
        for f in files {
            let text = fs::read_to_string(&f).unwrap_or_default();
            let _ = fs::remove_file(&f);
            if text.trim() == "quit" {
                self.log("asked to quit");
                self.quit = true;
                continue;
            }
            match parse_fired(&text, self.watch.ssid()) {
                Ok(e) => {
                    self.log(&format!("fired: {}", text.trim()));
                    out.push(e);
                }
                Err(e) => self.log(&format!("{}: {e}", f.display())),
            }
        }
        out
    }

    pub fn menu(&mut self, action: MenuAction, now: Timestamp) {
        let ds = self
            .config
            .as_ref()
            .map(|c| c.day_starts)
            .unwrap_or_default();
        let until = match action {
            MenuAction::Pause(len) => Some(now.checked_add(len).unwrap_or(now)),
            MenuAction::PauseToday => Some(ds.end(ds.day_of(now, &self.tz), &self.tz)),
            MenuAction::Resume => None,
            MenuAction::OpenConfig => {
                if let Err(e) = self.open_config() {
                    self.log(&e);
                }
                return;
            }
            MenuAction::Quit => {
                self.log("quit");
                return;
            }
        };
        match store::write_pause(&self.paths, until, &self.tz) {
            Ok(()) => {
                self.pause = until;
                self.log(&match until {
                    Some(t) => format!("paused until {}", store::local_rfc3339(t, &self.tz)),
                    None => "resumed".to_string(),
                });
            }
            Err(e) => self.log(&e),
        }
    }

    fn open_config(&mut self) -> Result<(), String> {
        let path = &self.paths.config;
        if !path.exists() {
            store::write_atomic(path, STARTER)?;
            self.log(&format!(
                "wrote a starting configuration to {}",
                path.display()
            ));
        }
        launch::edit(path)
    }

    pub fn view(&self, now: Timestamp) -> View {
        let paused = self.pause.is_some_and(|t| t > now);
        let headline = match (&self.problem, self.pause.filter(|t| *t > now)) {
            (Some(p), _) if self.config.is_none() => first_line(p),
            (Some(_), _) => "Configuration has an error".to_string(),
            (None, Some(t)) => format!("Paused until {}", short(t, now, &self.tz)),
            (None, None) => "Watching".to_string(),
        };
        let next = self.config.as_ref().and_then(|c| {
            c.rules
                .values()
                .filter(|r| r.enabled && r.is_timed())
                .filter_map(|r| {
                    r.next_period(now, c.day_starts, &self.tz)
                        .map(|p| (p, r.id.as_str()))
                })
                .filter(|(p, id)| !self.overrides.skips(id, p.day))
                .min_by_key(|(p, _)| p.start)
                .map(|(p, id)| format!("Next: {id}, {}", short(p.start, now, &self.tz)))
        });
        View {
            headline,
            next,
            paused,
            problem: self.problem.is_some(),
        }
    }

    /// Start what `launches` says, each in its own way; a process is waited
    /// for on a thread of its own so that its exit can be logged.
    pub fn run(&mut self, launches: Vec<Launch>) {
        let env = self
            .config
            .as_ref()
            .map(|c| c.env.clone())
            .unwrap_or_default();
        for l in launches {
            self.start_one(&l, &env);
        }
    }

    fn start_one(&mut self, l: &Launch, env: &BTreeMap<String, String>) {
        let period = l.period.display(&self.tz);
        match launch::start(&l.command, env, &Output::Log(self.paths.log())) {
            Ok(Started::HandedOff) => self.log(&format!("ran {} ({period})", l.rule)),
            Ok(Started::Child(mut child)) => {
                self.log(&format!("ran {} ({period}), pid {}", l.rule, child.id()));
                let (rule, log, tz) = (l.rule.clone(), self.paths.log(), self.tz.clone());
                std::thread::spawn(move || {
                    if let Ok(status) = child.wait()
                        && !status.success()
                    {
                        append(&log, &tz, &format!("{rule} exited with {status}"));
                    }
                });
            }
            Err(e) => self.log(&format!("could not run {}: {e}", l.rule)),
        }
    }

    pub fn log(&self, msg: &str) {
        append(&self.paths.log(), &self.tz, msg);
    }
}

fn append(path: &Path, tz: &TimeZone, msg: &str) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if fs::metadata(path).is_ok_and(|m| m.len() > LOG_LIMIT) {
        let _ = fs::rename(path, path.with_extension("log.1"));
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let when = store::local_rfc3339(Timestamp::now(), tz);
        for line in msg.lines() {
            let _ = writeln!(f, "{when} {line}");
        }
    }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or_default().to_string()
}

/// `15:30` today, `Tue 15:30` otherwise.
fn short(t: Timestamp, now: Timestamp, tz: &TimeZone) -> String {
    let z = t.to_zoned(tz.clone());
    if z.date() == now.to_zoned(tz.clone()).date() {
        z.strftime("%H:%M").to_string()
    } else {
        z.strftime("%a %H:%M").to_string()
    }
}

/// What `shiodoki fire` writes: `unlock`, `wake`, `login`, `network`, or
/// `network NAME`.
pub fn parse_fired(text: &str, ssid: Option<String>) -> Result<Event, String> {
    let mut words = text.split_whitespace();
    let event = match (words.next(), words.next()) {
        (Some("unlock"), None) => Event::Unlock,
        (Some("wake"), None) => Event::Wake,
        (Some("login"), None) => Event::Start {
            session: format!("fired-{}", Timestamp::now().as_millisecond()),
            ssid,
        },
        (Some("network"), name) => {
            let rest: Vec<&str> = name.into_iter().chain(words).collect();
            Event::Network {
                ssid: (!rest.is_empty()).then(|| rest.join(" ")).or(ssid),
            }
        }
        _ => return Err(format!("{:?} is not an event", text.trim())),
    };
    Ok(event)
}

const STARTER: &str = r#"# shiodoki configuration.  Rules are [rule.ID] tables; see the README:
# https://github.com/yoshzucker/shiodoki#a-configuration
#
# day_starts = "04:00"
#
# [rule.example]
# every = "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"
# at    = "10:00"
# until = "20m"
# open  = "https://example.com/"
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::local;
    use crate::watch::NetState;
    use jiff::civil::date;
    use std::time::Duration;

    struct T {
        _dir: tempfile::TempDir,
        agent: Agent,
        mono: u64,
    }

    fn agent(config: &str) -> T {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: dir.path().join("config.toml"),
            state_dir: dir.path().join("state"),
        };
        fs::write(&paths.config, config).unwrap();
        let agent = Agent::new(paths, Os::MacOS, TimeZone::fixed(jiff::tz::offset(9)));
        T {
            _dir: dir,
            agent,
            mono: 0,
        }
    }

    impl T {
        fn sample(&mut self, h: i8, m: i8, s: i8, locked: bool) -> Sample {
            self.mono += 2;
            Sample {
                wall: local(date(2026, 10, 13).at(h, m, s, 0), &self.agent.tz),
                mono: Duration::from_secs(self.mono),
                locked,
                net: NetState::default(),
            }
        }

        fn ids(launches: Vec<Launch>) -> Vec<String> {
            launches.into_iter().map(|l| l.rule).collect()
        }

        fn log(&self) -> String {
            fs::read_to_string(self.agent.paths.log()).unwrap_or_default()
        }
    }

    const REVIEW: &str = r#"
        [rule.review]
        every = "FREQ=WEEKLY;BYDAY=TU"
        at = "10:00"
        until = "20m"
        open = "https://meet.example.com/review"
        [rule.mount]
        on = ["login"]
        run = ["mount"]
    "#;

    #[test]
    fn a_login_then_the_lid_closed_on_the_way() {
        let mut t = agent(REVIEW);
        let s = t.sample(9, 0, 0, false);
        assert_eq!(T::ids(t.agent.begin(s, "console-1".into())), ["mount"]);
        let s = t.sample(9, 58, 0, true);
        assert!(t.agent.tick(s).is_empty());
        let s = t.sample(10, 0, 0, true);
        assert!(t.agent.tick(s).is_empty());
        let s = t.sample(10, 5, 0, false);
        assert_eq!(T::ids(t.agent.tick(s)), ["review"]);
        let log = t.log();
        assert!(log.contains("started in console-1"), "{log}");
        assert!(log.contains("review is due, and waits"), "{log}");
        assert!(log.contains("unlocked"), "{log}");
        // The state is saved, and a restart in the same session is no login.
        let saved = store::load_state(&t.agent.paths).unwrap();
        assert_eq!(saved.session.as_deref(), Some("console-1"));
        let mut again = Agent::new(t.agent.paths.clone(), Os::MacOS, t.agent.tz.clone());
        let s = t.sample(10, 6, 0, false);
        assert!(again.begin(s, "console-1".into()).is_empty());
    }

    #[test]
    fn a_broken_configuration_keeps_the_last_good_one() {
        let mut t = agent(REVIEW);
        let s = t.sample(9, 0, 0, false);
        t.agent.begin(s, "console-1".into());
        // A different length, so the change is seen whatever the clock's
        // resolution.
        fs::write(&t.agent.paths.config, "[rule.review]\nat = \"25:00:00:00\"").unwrap();
        let s = t.sample(10, 0, 0, false);
        assert_eq!(T::ids(t.agent.tick(s)), ["review"], "still the old rules");
        let v = {
            let w = t.sample(10, 0, 2, false).wall;
            t.agent.view(w)
        };
        assert_eq!(v.headline, "Configuration has an error");
        assert!(
            t.log()
                .contains("the last good configuration stays in force")
        );
    }

    #[test]
    fn no_configuration_yet() {
        let mut t = agent("");
        fs::remove_file(&t.agent.paths.config).unwrap();
        let s = t.sample(9, 0, 0, false);
        assert!(t.agent.begin(s, "console-1".into()).is_empty());
        let v = {
            let w = t.sample(9, 0, 2, false).wall;
            t.agent.view(w)
        };
        assert!(v.headline.starts_with("no configuration at"), "{v:?}");
        assert!(v.problem);
        fs::write(&t.agent.paths.config, REVIEW).unwrap();
        let s = t.sample(10, 0, 0, false);
        assert_eq!(
            T::ids(t.agent.tick(s)),
            ["mount", "review"],
            "the login is heard late, not lost"
        );
        assert!(
            !{
                let w = t.sample(10, 0, 2, false).wall;
                t.agent.view(w)
            }
            .problem
        );
    }

    #[test]
    fn the_menu_pauses_and_the_view_says_so() {
        let mut t = agent(REVIEW);
        let s = t.sample(9, 0, 0, false);
        t.agent.begin(s, "console-1".into());
        let now = t.sample(9, 50, 0, false).wall;
        assert_eq!(
            t.agent.view(now),
            View {
                headline: "Watching".into(),
                next: Some("Next: review, 10:00".into()),
                paused: false,
                problem: false
            }
        );
        t.agent
            .menu(MenuAction::Pause(SignedDuration::from_hours(2)), now);
        assert_eq!(t.agent.view(now).headline, "Paused until 11:50");
        let s = t.sample(10, 0, 0, false);
        assert!(t.agent.tick(s).is_empty(), "paused");
        t.agent.menu(MenuAction::Resume, now);
        let s = t.sample(10, 1, 0, false);
        assert_eq!(T::ids(t.agent.tick(s)), ["review"]);
        // A pause written by the command is picked up the same way.
        store::write_pause(
            &t.agent.paths,
            Some(now.checked_add(SignedDuration::from_hours(1)).unwrap()),
            &t.agent.tz,
        )
        .unwrap();
        let s = t.sample(10, 2, 0, false);
        t.agent.tick(s);
        assert!(t.agent.view(now).paused);
    }

    #[test]
    fn fired_events_arrive_through_the_inbox() {
        let mut t = agent(
            "[rule.u]\non = [\"unlock\"]\nrun = [\"x\"]\n[rule.n]\non = [\"network\"]\nssid = [\"Office\"]\nrun = [\"y\"]",
        );
        let s = t.sample(9, 0, 0, false);
        t.agent.begin(s, "console-1".into());
        let inbox = t.agent.paths.inbox();
        fs::create_dir_all(&inbox).unwrap();
        fs::write(inbox.join("1.event"), "unlock\n").unwrap();
        fs::write(inbox.join("2.event"), "network Office\n").unwrap();
        fs::write(inbox.join("3.event"), "lunch\n").unwrap();
        let s = t.sample(9, 1, 0, false);
        assert_eq!(T::ids(t.agent.tick(s)), ["u", "n"]);
        assert!(!t.agent.wants_quit());
        fs::write(inbox.join("4.event"), "quit\n").unwrap();
        let s = t.sample(9, 1, 2, false);
        t.agent.tick(s);
        assert!(t.agent.wants_quit());
        assert_eq!(fs::read_dir(&inbox).unwrap().count(), 0, "each read once");
        assert!(t.log().contains("\"lunch\" is not an event"));
    }

    #[test]
    fn fired_words() {
        assert_eq!(parse_fired("unlock", None), Ok(Event::Unlock));
        assert_eq!(
            parse_fired("network", Some("Home".into())),
            Ok(Event::Network {
                ssid: Some("Home".into())
            })
        );
        assert_eq!(
            parse_fired("network My Office", None),
            Ok(Event::Network {
                ssid: Some("My Office".into())
            })
        );
        assert!(matches!(
            parse_fired("login", None),
            Ok(Event::Start { .. })
        ));
        assert!(parse_fired("unlock now", None).is_err());
        assert!(parse_fired("", None).is_err());
    }
}

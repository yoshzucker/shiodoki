//! `config.toml`: the rules, read and checked.
//!
//! Every problem in the file is collected rather than the first one alone,
//! so that `shiodoki check` can say everything that is wrong at once.  A key
//! nobody reads is a problem, not something quietly ignored.

use std::collections::BTreeMap;
use std::fmt;

use jiff::SignedDuration;
use jiff::civil::{Date, Time};
use serde::{Deserialize, Serialize};
use toml::{Table, Value};

use crate::clock::{DayStart, parse_length, parse_time};
use crate::rrule::Schedule;

pub type RuleId = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventKind {
    Login,
    Unlock,
    Wake,
    Network,
}

impl EventKind {
    pub fn parse(s: &str) -> Option<EventKind> {
        Some(match s {
            "login" => EventKind::Login,
            "unlock" => EventKind::Unlock,
            "wake" => EventKind::Wake,
            "network" => EventKind::Network,
            _ => return None,
        })
    }
}

/// When a period closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Until {
    /// The next time the clock reads this.
    At(Time),
    /// This long after the period opens.
    After(SignedDuration),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Run {
        argv: Vec<String>,
        window: bool,
    },
    Open {
        target: String,
        with: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub id: RuleId,
    pub schedule: Schedule,
    pub at: Option<Time>,
    pub until: Option<Until>,
    /// Empty for a timed rule.
    pub on: Vec<EventKind>,
    pub once: bool,
    /// The Wi-Fi networks the rule runs on; empty for any.
    pub ssid: Vec<String>,
    pub unlocked: bool,
    pub except: Vec<Date>,
    pub enabled: bool,
    pub command: Command,
}

impl Rule {
    pub fn is_timed(&self) -> bool {
        self.on.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub day_starts: DayStart,
    pub env: BTreeMap<String, String>,
    pub rules: BTreeMap<RuleId, Rule>,
}

/// The OS a configuration is read for: `[rule.ID.macos]` and
/// `[rule.ID.windows]` apply only on their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    MacOS,
    Windows,
    Other,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(target_os = "macos") {
            Os::MacOS
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Other
        }
    }

    fn key(self) -> Option<&'static str> {
        match self {
            Os::MacOS => Some("macos"),
            Os::Windows => Some("windows"),
            Os::Other => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError(pub Vec<String>);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, p) in self.0.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{p}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigError {}

const RULE_KEYS: &[&str] = &[
    "every", "from", "at", "until", "on", "once", "ssid", "unlocked", "except", "enabled", "run",
    "open", "with", "window",
];
const COMMAND_KEYS: &[&str] = &["run", "open", "with", "window"];
const OS_KEYS: &[&str] = &["macos", "windows"];

impl Config {
    pub fn parse(text: &str, os: Os) -> Result<Config, ConfigError> {
        let table: Table = toml::from_str(text).map_err(|e| ConfigError(vec![e.to_string()]))?;
        let mut problems = vec![];
        let mut config = Config {
            day_starts: DayStart::default(),
            env: BTreeMap::new(),
            rules: BTreeMap::new(),
        };
        for (key, value) in &table {
            match key.as_str() {
                "day_starts" => match time_value(value) {
                    Ok(t) => config.day_starts = DayStart(t),
                    Err(e) => problems.push(format!("day_starts: {e}")),
                },
                "env" => match value.as_table() {
                    Some(t) => {
                        for (k, v) in t {
                            match v.as_str() {
                                Some(s) => {
                                    config.env.insert(k.clone(), s.to_string());
                                }
                                None => problems.push(format!("env.{k}: expected a string")),
                            }
                        }
                    }
                    None => problems.push("env: expected a table of strings".into()),
                },
                "rule" => match value.as_table() {
                    Some(t) => {
                        for (id, v) in t {
                            match parse_rule(id, v, os) {
                                Ok(r) => {
                                    config.rules.insert(id.clone(), r);
                                }
                                Err(ps) => problems.extend(ps),
                            }
                        }
                    }
                    None => problems.push("rule: expected [rule.ID] tables".into()),
                },
                _ => problems.push(format!("{key}: not a setting")),
            }
        }
        if problems.is_empty() {
            Ok(config)
        } else {
            Err(ConfigError(problems))
        }
    }
}

fn parse_rule(id: &str, value: &Value, os: Os) -> Result<Rule, Vec<String>> {
    let at_rule = |key: &str, msg: String| format!("rule.{id}.{key}: {msg}");
    let Some(table) = value.as_table() else {
        return Err(vec![format!("rule.{id}: expected a table")]);
    };
    let mut problems = vec![];
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        problems.push(format!("rule.{id}: an ID is letters, digits, '-' and '_'"));
    }

    // The rule's own keys, with those of this OS's table laid over them.
    // A `run` or `open` there replaces how the rule runs things as a whole.
    let mut merged = Table::new();
    for (k, v) in table {
        if OS_KEYS.contains(&k.as_str()) {
            let Some(sub) = v.as_table() else {
                problems.push(format!("rule.{id}.{k}: expected a table"));
                continue;
            };
            for sk in sub.keys() {
                if !RULE_KEYS.contains(&sk.as_str()) {
                    problems.push(format!("rule.{id}.{k}.{sk}: not a rule setting"));
                }
            }
        } else if RULE_KEYS.contains(&k.as_str()) {
            merged.insert(k.clone(), v.clone());
        } else {
            problems.push(format!("rule.{id}.{k}: not a rule setting"));
        }
    }
    if let Some(sub) = os
        .key()
        .and_then(|k| table.get(k))
        .and_then(Value::as_table)
    {
        if sub.keys().any(|k| k == "run" || k == "open") {
            for k in COMMAND_KEYS {
                merged.remove(*k);
            }
        }
        for (k, v) in sub {
            merged.insert(k.clone(), v.clone());
        }
    }

    let get = |key: &str| merged.get(key).cloned();
    let note = |problems: &mut Vec<String>, key: &str, r: Result<(), String>| {
        if let Err(e) = r {
            problems.push(format!("rule.{id}.{key}: {e}"));
        }
    };
    macro_rules! field {
        ($key:expr, $r:expr $(,)?) => {{
            let r = $r;
            note(&mut problems, $key, r)
        }};
    }

    let mut from = None;
    if let Some(v) = get("from") {
        field!("from", date_value(&v).map(|d| from = Some(d)));
    }
    let mut schedule = None;
    let every = match get("every") {
        None => Ok("FREQ=DAILY".to_string()),
        Some(v) => v
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "expected an RRULE string".to_string()),
    };
    field!(
        if get("every").is_some() {
            "every"
        } else {
            "from"
        },
        every
            .and_then(|e| Schedule::parse(&e, from))
            .map(|s| schedule = Some(s))
    );
    let mut at = None;
    if let Some(v) = get("at") {
        field!("at", time_value(&v).map(|t| at = Some(t)));
    }
    let mut until = None;
    if let Some(v) = get("until") {
        field!("until", until_value(&v).map(|u| until = Some(u)));
    }
    let mut on = vec![];
    if let Some(v) = get("on") {
        field!(
            "on",
            strings(&v).and_then(|names| {
                for n in names {
                    let k = EventKind::parse(&n).ok_or_else(|| {
                        format!("{n:?} is not an event (login, unlock, wake, network)")
                    })?;
                    if !on.contains(&k) {
                        on.push(k);
                    }
                }
                if on.is_empty() {
                    Err("an empty list never fires; leave `on` out for a timed rule".into())
                } else {
                    Ok(())
                }
            }),
        );
    }
    let mut once = false;
    if let Some(v) = get("once") {
        field!("once", boolean(&v).map(|b| once = b));
    }
    let mut ssid = vec![];
    if let Some(v) = get("ssid") {
        field!("ssid", strings(&v).map(|s| ssid = s));
    }
    let mut unlocked = true;
    if let Some(v) = get("unlocked") {
        field!("unlocked", boolean(&v).map(|b| unlocked = b));
    }
    let mut except = vec![];
    if let Some(v) = get("except") {
        field!(
            "except",
            match &v {
                Value::Array(a) => a
                    .iter()
                    .map(date_value)
                    .collect::<Result<Vec<_>, _>>()
                    .map(|d| except = d),
                other => date_value(other).map(|d| except = vec![d]),
            },
        );
    }
    let mut enabled = true;
    if let Some(v) = get("enabled") {
        field!("enabled", boolean(&v).map(|b| enabled = b));
    }

    let mut command = None;
    match (get("run"), get("open")) {
        (Some(_), Some(_)) => problems.push(format!("rule.{id}: give `run` or `open`, not both")),
        (None, None) => problems.push(format!("rule.{id}: nothing to do -- give `run` or `open`")),
        (Some(run), None) => {
            let mut window = false;
            if let Some(v) = get("window") {
                field!("window", boolean(&v).map(|b| window = b));
            }
            if get("with").is_some() {
                problems.push(at_rule("with", "goes with `open`, not `run`".into()));
            }
            field!(
                "run",
                strings(&run).and_then(|argv| {
                    if argv.is_empty() || argv[0].is_empty() {
                        Err("expected a command and its arguments".into())
                    } else {
                        command = Some(Command::Run { argv, window });
                        Ok(())
                    }
                }),
            );
        }
        (None, Some(open)) => {
            let mut with = None;
            if let Some(v) = get("with") {
                field!(
                    "with",
                    v.as_str()
                        .map(|s| with = Some(s.to_string()))
                        .ok_or("expected a string".into())
                );
            }
            if get("window").is_some() {
                problems.push(at_rule("window", "goes with `run`, not `open`".into()));
            }
            field!(
                "open",
                open.as_str()
                    .filter(|s| !s.is_empty())
                    .map(|s| command = Some(Command::Open {
                        target: s.to_string(),
                        with
                    }))
                    .ok_or("expected a URL or a path".into()),
            );
        }
    }

    if on.is_empty() && once {
        problems.push(at_rule(
            "once",
            "only means something with `on`; a timed rule runs once a period anyway".into(),
        ));
    }
    match (problems.is_empty(), schedule, command) {
        (true, Some(schedule), Some(command)) => Ok(Rule {
            id: id.to_string(),
            schedule,
            at,
            until,
            on,
            once,
            ssid,
            unlocked,
            except,
            enabled,
            command,
        }),
        _ => Err(problems),
    }
}

fn boolean(v: &Value) -> Result<bool, String> {
    v.as_bool().ok_or_else(|| "expected true or false".into())
}

/// A list of strings, or one string standing for a list of one.
fn strings(v: &Value) -> Result<Vec<String>, String> {
    match v {
        Value::String(s) => Ok(vec![s.clone()]),
        Value::Array(a) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "expected strings".to_string())
            })
            .collect(),
        _ => Err("expected a list of strings".into()),
    }
}

/// `2026-10-08`, written either as a TOML date or as a string.
pub fn date_value(v: &Value) -> Result<Date, String> {
    match v {
        Value::Datetime(dt) => match (dt.date, dt.time, dt.offset) {
            (Some(d), None, None) => {
                Date::new(d.year as i16, d.month as i8, d.day as i8).map_err(|e| e.to_string())
            }
            _ => Err(format!("{dt} is not a plain date")),
        },
        Value::String(s) => s
            .parse::<Date>()
            .map_err(|_| format!("{s:?} is not a date (expected YYYY-MM-DD)")),
        _ => Err("expected a date such as 2026-10-08".into()),
    }
}

/// `"10:00"`, or a TOML local time.
pub fn time_value(v: &Value) -> Result<Time, String> {
    match v {
        Value::String(s) => parse_time(s),
        Value::Datetime(dt) => match (dt.date, dt.time, dt.offset) {
            (None, Some(t), None) => {
                Time::new(t.hour as i8, t.minute as i8, t.second.unwrap_or(0) as i8, 0)
                    .map_err(|e| e.to_string())
            }
            _ => Err(format!("{dt} is not a time of day")),
        },
        _ => Err("expected a time of day such as \"10:00\"".into()),
    }
}

fn until_value(v: &Value) -> Result<Until, String> {
    match v {
        Value::String(s) if s.contains(':') => parse_time(s).map(Until::At),
        Value::String(s) => parse_length(s).map(Until::After),
        Value::Datetime(_) => time_value(v).map(Until::At),
        _ => Err("expected a time of day (\"10:20\") or a length (\"20m\")".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::{date, time};

    fn readme_example() -> &'static str {
        let readme = include_str!("../README.md");
        let start = readme
            .find("```toml\n")
            .expect("a toml block in the README")
            + 8;
        let len = readme[start..].find("```").unwrap();
        &readme[start..start + len]
    }

    fn problems(text: &str) -> Vec<String> {
        Config::parse(text, Os::MacOS).unwrap_err().0
    }

    #[test]
    fn the_readme_example_reads() {
        for os in [Os::MacOS, Os::Windows] {
            let c = Config::parse(readme_example(), os).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(c.day_starts, DayStart(time(4, 0, 0, 0)));
            let r = &c.rules["weekly-review"];
            assert!(r.is_timed());
            assert_eq!(r.at, Some(time(10, 0, 0, 0)));
            assert_eq!(r.until, Some(Until::After(SignedDuration::from_mins(20))));
            assert_eq!(
                r.command,
                Command::Open {
                    target: "https://meet.example.com/weekly-review".into(),
                    with: Some("Firefox".into())
                }
            );
            let m = &c.rules["morning-capture"];
            assert_eq!(
                m.on,
                vec![EventKind::Login, EventKind::Unlock, EventKind::Wake]
            );
            assert!(m.once);
            assert_eq!(m.until, Some(Until::At(time(12, 0, 0, 0))));
            let v = &c.rules["office-share"];
            assert_eq!(v.ssid, vec!["Example-Office".to_string()]);
            assert!(!v.unlocked);
        }
    }

    #[test]
    fn an_os_table_replaces_how_a_rule_runs() {
        let text = r#"
            [rule.r]
            open = "https://example.com"
            with = "Firefox"
            [rule.r.windows]
            run = ["C:\\tools\\x.exe"]
            [rule.r.macos]
            with = "Safari"
        "#;
        let win = Config::parse(text, Os::Windows).unwrap();
        assert_eq!(
            win.rules["r"].command,
            Command::Run {
                argv: vec![r"C:\tools\x.exe".into()],
                window: false
            }
        );
        let mac = Config::parse(text, Os::MacOS).unwrap();
        assert_eq!(
            mac.rules["r"].command,
            Command::Open {
                target: "https://example.com".into(),
                with: Some("Safari".into())
            }
        );
        let other = Config::parse(text, Os::Other).unwrap();
        assert_eq!(
            other.rules["r"].command,
            Command::Open {
                target: "https://example.com".into(),
                with: Some("Firefox".into())
            }
        );
    }

    #[test]
    fn values_in_either_spelling() {
        let c = Config::parse(
            r#"
            day_starts = 04:30:00
            [rule.a]
            every = "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO"
            from = "2026-10-05"
            at = 09:00:00
            until = 09:30:00
            except = 2026-10-19
            run = "true"
            "#,
            Os::MacOS,
        )
        .unwrap();
        assert_eq!(c.day_starts, DayStart(time(4, 30, 0, 0)));
        let a = &c.rules["a"];
        assert_eq!(a.until, Some(Until::At(time(9, 30, 0, 0))));
        assert_eq!(a.except, vec![date(2026, 10, 19)]);
        assert_eq!(
            a.command,
            Command::Run {
                argv: vec!["true".into()],
                window: false
            }
        );
    }

    #[test]
    fn every_problem_is_reported() {
        let ps = problems(
            r#"
            colour = "blue"
            [rule.a]
            at = "25:00"
            run = ["x"]
            open = "y"
            [rule.b]
            every = "FREQ=WEEKLY;INTERVAL=2;BYDAY=TU"
            once = true
            ssid = "Home"
            open = "https://example.com"
            window = true
            [rule."has space"]
            run = ["x"]
            [rule.c]
            on = ["login", "lunch"]
            run = []
            [rule.d]
            open = "x"
            surprise = 1
            [rule.d.linux]
            run = ["y"]
            "#,
        );
        let has = |needle: &str| {
            assert!(
                ps.iter().any(|p| p.contains(needle)),
                "no {needle:?} in {ps:#?}"
            )
        };
        has("colour: not a setting");
        has("rule.a.at");
        has("rule.a: give `run` or `open`, not both");
        has("rule.b.every: `from` is required");
        has("rule.b.once");
        has("rule.b.window");
        has("rule.has space: an ID");
        has("rule.c.on: \"lunch\" is not an event");
        has("rule.c.run");
        has("rule.d.surprise");
        has("rule.d.linux");
    }

    #[test]
    fn a_toml_syntax_error_is_one_problem() {
        assert_eq!(problems("[rule.a\nrun = 1").len(), 1);
    }
}

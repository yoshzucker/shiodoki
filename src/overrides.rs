//! `overrides.toml`: skips and blocks, kept next to the configuration.
//!
//! The commands write this file and a person may edit it too, so it is
//! plain TOML in local time:
//!
//! ```toml
//! [[skip]]
//! rule = "weekly-review"
//! on = 2026-10-13            # or from = ... and to = ..., both included
//!
//! [[block]]
//! from = 2026-10-15T13:00:00
//! to = 2026-10-15T15:00:00
//! rules = ["retro"]          # leave out for every rule
//! ```

use jiff::Timestamp;
use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use toml::{Table, Value};

use crate::clock::local;
use crate::config::{RuleId, date_value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skip {
    pub rule: RuleId,
    pub from: Date,
    pub to: Date,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub from: Timestamp,
    pub to: Timestamp,
    /// Empty for every rule.
    pub rules: Vec<RuleId>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overrides {
    pub skips: Vec<Skip>,
    pub blocks: Vec<Block>,
}

impl Overrides {
    pub fn skips(&self, rule: &str, day: Date) -> bool {
        self.skips
            .iter()
            .any(|s| s.rule == rule && s.from <= day && day <= s.to)
    }

    pub fn blocks(&self, rule: &str, now: Timestamp) -> bool {
        self.blocks.iter().any(|b| {
            b.from <= now && now < b.to && (b.rules.is_empty() || b.rules.iter().any(|r| r == rule))
        })
    }

    /// Drop what can no longer matter: skips of days before `today`, blocks
    /// that have ended.
    pub fn prune(&mut self, now: Timestamp, today: Date) {
        self.skips.retain(|s| s.to >= today);
        self.blocks.retain(|b| b.to > now);
    }

    pub fn parse(text: &str, tz: &TimeZone) -> Result<Overrides, Vec<String>> {
        let table: Table = toml::from_str(text).map_err(|e| vec![e.to_string()])?;
        let mut out = Overrides::default();
        let mut problems = vec![];
        for (key, value) in &table {
            let entries = match value.as_array() {
                Some(a) => a,
                None => {
                    problems.push(format!("{key}: expected [[{key}]] entries"));
                    continue;
                }
            };
            for (i, e) in entries.iter().enumerate() {
                let here = format!("{key}[{}]", i + 1);
                let Some(t) = e.as_table() else {
                    problems.push(format!("{here}: expected a table"));
                    continue;
                };
                match key.as_str() {
                    "skip" => match parse_skip(t) {
                        Ok(s) => out.skips.push(s),
                        Err(e) => problems.push(format!("{here}: {e}")),
                    },
                    "block" => match parse_block(t, tz) {
                        Ok(b) => out.blocks.push(b),
                        Err(e) => problems.push(format!("{here}: {e}")),
                    },
                    _ => {
                        problems.push(format!("{key}: not skip or block"));
                        break;
                    }
                }
            }
        }
        if problems.is_empty() {
            Ok(out)
        } else {
            Err(problems)
        }
    }

    pub fn to_toml(&self, tz: &TimeZone) -> String {
        let mut s = String::new();
        for k in &self.skips {
            s.push_str(&format!(
                "[[skip]]\nrule = {}\n",
                Value::from(k.rule.clone())
            ));
            if k.from == k.to {
                s.push_str(&format!("on = {}\n\n", k.from));
            } else {
                s.push_str(&format!("from = {}\nto = {}\n\n", k.from, k.to));
            }
        }
        for b in &self.blocks {
            let fmt = |t: Timestamp| tz.to_datetime(t).strftime("%Y-%m-%dT%H:%M:%S").to_string();
            s.push_str(&format!(
                "[[block]]\nfrom = {}\nto = {}\n",
                fmt(b.from),
                fmt(b.to)
            ));
            if !b.rules.is_empty() {
                let rules: Vec<String> = b
                    .rules
                    .iter()
                    .map(|r| Value::from(r.clone()).to_string())
                    .collect();
                s.push_str(&format!("rules = [{}]\n", rules.join(", ")));
            }
            s.push('\n');
        }
        s
    }
}

fn known(t: &Table, keys: &[&str]) -> Result<(), String> {
    match t.keys().find(|k| !keys.contains(&k.as_str())) {
        Some(k) => Err(format!("{k}: not one of {}", keys.join(", "))),
        None => Ok(()),
    }
}

fn parse_skip(t: &Table) -> Result<Skip, String> {
    known(t, &["rule", "on", "from", "to"])?;
    let rule = t
        .get("rule")
        .and_then(Value::as_str)
        .ok_or("rule: expected a rule ID")?
        .to_string();
    let day = |k: &str| {
        t.get(k)
            .map(|v| date_value(v).map_err(|e| format!("{k}: {e}")))
            .transpose()
    };
    let (from, to) = match (day("on")?, day("from")?, day("to")?) {
        (Some(d), None, None) => (d, d),
        (None, Some(f), Some(u)) if f <= u => (f, u),
        (None, Some(_), Some(_)) => return Err("`from` is after `to`".into()),
        _ => return Err("give `on`, or `from` and `to`".into()),
    };
    Ok(Skip { rule, from, to })
}

fn parse_block(t: &Table, tz: &TimeZone) -> Result<Block, String> {
    known(t, &["from", "to", "rules"])?;
    let when = |k: &str| -> Result<Timestamp, String> {
        let v = t.get(k).ok_or_else(|| format!("{k}: missing"))?;
        datetime_value(v, tz).map_err(|e| format!("{k}: {e}"))
    };
    let (from, to) = (when("from")?, when("to")?);
    if from >= to {
        return Err("`from` is not before `to`".into());
    }
    let rules = match t.get("rules") {
        None => vec![],
        Some(Value::Array(a)) => a
            .iter()
            .map(|r| {
                r.as_str()
                    .map(str::to_string)
                    .ok_or("rules: expected rule IDs".to_string())
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("rules: expected a list of rule IDs".into()),
    };
    Ok(Block { from, to, rules })
}

/// A local date and time (`2026-10-15T13:00:00`), or one with an offset.
fn datetime_value(v: &Value, tz: &TimeZone) -> Result<Timestamp, String> {
    let text = match v {
        Value::Datetime(dt) => dt.to_string(),
        Value::String(s) => s.clone(),
        _ => return Err("expected a date and time".into()),
    };
    if let Ok(t) = text.parse::<Timestamp>() {
        return Ok(t);
    }
    text.parse::<DateTime>()
        .map(|dt| local(dt, tz))
        .map_err(|_| format!("{text:?} is not a date and time (expected 2026-10-15T13:00:00)"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn tz() -> TimeZone {
        TimeZone::fixed(jiff::tz::offset(9))
    }

    #[test]
    fn reads_the_documented_form() {
        let o = Overrides::parse(
            r#"
            [[skip]]
            rule = "weekly-review"
            on = 2026-10-13
            [[skip]]
            rule = "retro"
            from = 2026-10-01
            to = 2026-10-31
            [[block]]
            from = 2026-10-15T13:00:00
            to = 2026-10-15T15:00:00
            rules = ["retro"]
            [[block]]
            from = 2026-10-16T13:00
            to = "2026-10-16T14:00:00+09:00"
            "#,
            &tz(),
        )
        .unwrap();
        assert!(o.skips("weekly-review", date(2026, 10, 13)));
        assert!(!o.skips("weekly-review", date(2026, 10, 20)));
        assert!(o.skips("retro", date(2026, 10, 22)));
        let at = |d: Date, h, m| local(d.at(h, m, 0, 0), &tz());
        assert!(o.blocks("retro", at(date(2026, 10, 15), 13, 0)));
        assert!(!o.blocks("retro", at(date(2026, 10, 15), 15, 0)));
        assert!(!o.blocks("weekly-review", at(date(2026, 10, 15), 14, 0)));
        assert!(o.blocks("weekly-review", at(date(2026, 10, 16), 13, 30)));
    }

    #[test]
    fn round_trips() {
        let at = |d: Date, h, m| local(d.at(h, m, 0, 0), &tz());
        let o = Overrides {
            skips: vec![
                Skip {
                    rule: "a".into(),
                    from: date(2026, 10, 13),
                    to: date(2026, 10, 13),
                },
                Skip {
                    rule: "b".into(),
                    from: date(2026, 10, 1),
                    to: date(2026, 10, 9),
                },
            ],
            blocks: vec![
                Block {
                    from: at(date(2026, 10, 15), 13, 0),
                    to: at(date(2026, 10, 15), 15, 0),
                    rules: vec![],
                },
                Block {
                    from: at(date(2026, 10, 16), 9, 0),
                    to: at(date(2026, 10, 17), 4, 0),
                    rules: vec!["a".into()],
                },
            ],
        };
        assert_eq!(Overrides::parse(&o.to_toml(&tz()), &tz()).unwrap(), o);
        assert_eq!(Overrides::parse("", &tz()).unwrap(), Overrides::default());
    }

    #[test]
    fn prunes_what_is_over() {
        let at = |d: Date, h, m| local(d.at(h, m, 0, 0), &tz());
        let mut o = Overrides {
            skips: vec![
                Skip {
                    rule: "a".into(),
                    from: date(2026, 10, 1),
                    to: date(2026, 10, 6),
                },
                Skip {
                    rule: "a".into(),
                    from: date(2026, 10, 1),
                    to: date(2026, 10, 7),
                },
            ],
            blocks: vec![
                Block {
                    from: at(date(2026, 10, 7), 9, 0),
                    to: at(date(2026, 10, 7), 10, 0),
                    rules: vec![],
                },
                Block {
                    from: at(date(2026, 10, 7), 9, 0),
                    to: at(date(2026, 10, 7), 11, 0),
                    rules: vec![],
                },
            ],
        };
        o.prune(at(date(2026, 10, 7), 10, 0), date(2026, 10, 7));
        assert_eq!(o.skips.len(), 1);
        assert_eq!(o.blocks.len(), 1);
    }

    #[test]
    fn refuses_what_it_cannot_read() {
        for bad in [
            "[[skip]]\non = 2026-10-13",
            "[[skip]]\nrule = \"a\"",
            "[[skip]]\nrule = \"a\"\nfrom = 2026-10-13\nto = 2026-10-01",
            "[[skip]]\nrule = \"a\"\non = 2026-10-13\nwhy = \"holiday\"",
            "[[block]]\nfrom = 2026-10-15T15:00:00\nto = 2026-10-15T13:00:00",
            "[[block]]\nfrom = 2026-10-15T13:00:00",
            "[[pause]]\nuntil = 2026-10-15T13:00:00",
            "skip = 1",
        ] {
            assert!(Overrides::parse(bad, &tz()).is_err(), "{bad}");
        }
    }
}

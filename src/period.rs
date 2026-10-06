//! Periods: the stretches of time a rule may run in.
//!
//! A rule's schedule chooses days; `at` and `until` turn each day into a
//! period.  A period also ends when the rule's next one opens, so two
//! periods of one rule never overlap and "the period now" is always one or
//! none.

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::clock::{DayStart, add_days, next_reading};
use crate::config::{Rule, Until};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Period {
    /// The day the schedule chose -- what `skip` and `except` name.
    pub day: Date,
    pub start: Timestamp,
    pub end: Timestamp,
}

impl Period {
    pub fn contains(&self, t: Timestamp) -> bool {
        self.start <= t && t < self.end
    }
}

/// How far ahead to look for a rule's next day before giving up on it.
const HORIZON: usize = 1000;

impl Rule {
    /// Whether the rule has a period on `d`.
    pub fn falls_on(&self, d: Date) -> bool {
        !self.except.contains(&d) && !self.schedule.between(d, d).is_empty()
    }

    /// The first day after `d` the rule has a period on.
    fn next_day(&self, d: Date) -> Option<Date> {
        let mut cur = d;
        for _ in 0..HORIZON {
            cur = self.schedule.first_after(cur)?;
            if !self.except.contains(&cur) {
                return Some(cur);
            }
        }
        None
    }

    fn opening(&self, day: Date, ds: DayStart, tz: &TimeZone) -> Timestamp {
        ds.at(day, self.at.unwrap_or(ds.0), tz)
    }

    /// The period on `day`, which the caller knows the rule falls on.
    pub fn period_on(&self, day: Date, ds: DayStart, tz: &TimeZone) -> Period {
        let start = self.opening(day, ds, tz);
        let natural = match self.until {
            Some(Until::At(t)) => next_reading(start, t, tz),
            Some(Until::After(len)) => start.checked_add(len).expect("a period that ends"),
            None => ds.end(day, tz),
        };
        let end = match self.next_day(day) {
            Some(n) => natural.min(self.opening(n, ds, tz)),
            None => natural,
        };
        Period { day, start, end }
    }

    /// How many days before `now`'s a period containing `now` may have
    /// opened.
    fn lookback(&self) -> i64 {
        match self.until {
            Some(Until::After(len)) => len.as_secs() / 86_400 + 2,
            _ => 2,
        }
    }

    /// The period `now` is in, if any.
    pub fn period_at(&self, now: Timestamp, ds: DayStart, tz: &TimeZone) -> Option<Period> {
        let today = ds.day_of(now, tz);
        let days = self
            .schedule
            .between(add_days(today, -self.lookback()), today);
        days.into_iter()
            .rev()
            .filter(|d| !self.except.contains(d))
            .map(|d| self.period_on(d, ds, tz))
            .find(|p| p.start <= now)
            .filter(|p| p.contains(now))
    }

    /// The first period that opens after `now`.
    pub fn next_period(&self, now: Timestamp, ds: DayStart, tz: &TimeZone) -> Option<Period> {
        let mut day = ds.day_of(now, tz);
        if !self.falls_on(day) {
            day = self.next_day(day)?;
        }
        for _ in 0..HORIZON {
            let p = self.period_on(day, ds, tz);
            if p.start > now {
                return Some(p);
            }
            day = self.next_day(day)?;
        }
        None
    }

    /// The periods that open at or after `from`, in order, up to `n` of them.
    pub fn upcoming(&self, from: Timestamp, n: usize, ds: DayStart, tz: &TimeZone) -> Vec<Period> {
        let mut out = vec![];
        let mut t = from
            .checked_sub(SignedDuration::from_nanos(1))
            .unwrap_or(from);
        while out.len() < n {
            let Some(p) = self.next_period(t, ds, tz) else {
                break;
            };
            t = p.start;
            out.push(p);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::local;
    use crate::config::{Config, Os};
    use jiff::civil::{DateTime, date};

    fn tz() -> TimeZone {
        TimeZone::fixed(jiff::tz::offset(9))
    }

    fn ts(dt: DateTime) -> Timestamp {
        local(dt, &tz())
    }

    fn rule(body: &str) -> (Rule, DayStart) {
        let c = Config::parse(
            &format!("day_starts = \"04:00\"\n[rule.r]\nrun = [\"x\"]\n{body}"),
            Os::MacOS,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        (c.rules["r"].clone(), c.day_starts)
    }

    #[test]
    fn a_timed_period() {
        let (r, ds) = rule("every = \"FREQ=WEEKLY;BYDAY=TU\"\nat = \"10:00\"\nuntil = \"20m\"");
        let p = r
            .period_at(ts(date(2026, 10, 13).at(10, 5, 0, 0)), ds, &tz())
            .unwrap();
        assert_eq!(p.day, date(2026, 10, 13));
        assert_eq!(p.start, ts(date(2026, 10, 13).at(10, 0, 0, 0)));
        assert_eq!(p.end, ts(date(2026, 10, 13).at(10, 20, 0, 0)));
        assert_eq!(
            r.period_at(ts(date(2026, 10, 13).at(10, 20, 0, 0)), ds, &tz()),
            None
        );
        assert_eq!(
            r.period_at(ts(date(2026, 10, 13).at(9, 59, 0, 0)), ds, &tz()),
            None
        );
        assert_eq!(
            r.period_at(ts(date(2026, 10, 14).at(10, 5, 0, 0)), ds, &tz()),
            None
        );
        let n = r
            .next_period(ts(date(2026, 10, 13).at(10, 0, 0, 0)), ds, &tz())
            .unwrap();
        assert_eq!(n.day, date(2026, 10, 20));
    }

    #[test]
    fn a_day_by_default() {
        let (r, ds) = rule("on = [\"login\"]");
        let p = r
            .period_at(ts(date(2026, 10, 8).at(2, 0, 0, 0)), ds, &tz())
            .unwrap();
        // Two in the morning is still the 7th's.
        assert_eq!(p.day, date(2026, 10, 7));
        assert_eq!(p.start, ts(date(2026, 10, 7).at(4, 0, 0, 0)));
        assert_eq!(p.end, ts(date(2026, 10, 8).at(4, 0, 0, 0)));
    }

    #[test]
    fn until_a_time_past_midnight() {
        let (r, ds) = rule("at = \"23:00\"\nuntil = \"01:00\"");
        let p = r
            .period_at(ts(date(2026, 10, 8).at(0, 30, 0, 0)), ds, &tz())
            .unwrap();
        assert_eq!(p.day, date(2026, 10, 7));
        assert_eq!(p.end, ts(date(2026, 10, 8).at(1, 0, 0, 0)));
    }

    #[test]
    fn a_period_ends_when_the_next_one_opens() {
        let (r, ds) = rule("at = \"10:00\"\nuntil = \"30h\"");
        let p = r
            .period_at(ts(date(2026, 10, 9).at(9, 0, 0, 0)), ds, &tz())
            .unwrap();
        assert_eq!(p.day, date(2026, 10, 8));
        assert_eq!(p.end, ts(date(2026, 10, 9).at(10, 0, 0, 0)));
        let q = r
            .period_at(ts(date(2026, 10, 9).at(11, 0, 0, 0)), ds, &tz())
            .unwrap();
        assert_eq!(q.day, date(2026, 10, 9));
    }

    #[test]
    fn a_long_until_reaches_back_past_skipped_days() {
        // Mondays, open until Wednesday noon.
        let (r, ds) = rule("every = \"FREQ=WEEKLY;BYDAY=MO\"\nat = \"09:00\"\nuntil = \"51h\"");
        let p = r
            .period_at(ts(date(2026, 10, 7).at(11, 0, 0, 0)), ds, &tz())
            .unwrap();
        assert_eq!(p.day, date(2026, 10, 5));
        assert_eq!(
            r.period_at(ts(date(2026, 10, 7).at(12, 0, 0, 0)), ds, &tz()),
            None
        );
    }

    #[test]
    fn except_takes_a_day_out() {
        let (r, ds) =
            rule("every = \"FREQ=WEEKLY;BYDAY=TU\"\nat = \"10:00\"\nexcept = [2026-10-13]");
        assert_eq!(
            r.period_at(ts(date(2026, 10, 13).at(10, 5, 0, 0)), ds, &tz()),
            None
        );
        // The period before it is not cut short by a day that never opens.
        let p = r
            .period_at(ts(date(2026, 10, 6).at(23, 0, 0, 0)), ds, &tz())
            .unwrap();
        assert_eq!(p.end, ts(date(2026, 10, 7).at(4, 0, 0, 0)));
        let n = r
            .next_period(ts(date(2026, 10, 7).at(0, 0, 0, 0)), ds, &tz())
            .unwrap();
        assert_eq!(n.day, date(2026, 10, 20));
    }

    #[test]
    fn upcoming_lists_in_order() {
        let (r, ds) = rule("every = \"FREQ=WEEKLY;BYDAY=TU,TH\"\nat = \"10:00\"");
        let days: Vec<Date> = r
            .upcoming(ts(date(2026, 10, 13).at(10, 0, 0, 0)), 3, ds, &tz())
            .iter()
            .map(|p| p.day)
            .collect();
        assert_eq!(
            days,
            vec![date(2026, 10, 13), date(2026, 10, 15), date(2026, 10, 20)]
        );
    }
}

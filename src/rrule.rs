//! The part of an RFC 5545 RRULE that chooses days.
//!
//! The time of day is the rule's `at`, so the parts that choose times --
//! `BYHOUR`, `BYMINUTE`, `BYSECOND`, and the frequencies finer than a day --
//! are refused rather than ignored.  So are `BYYEARDAY` and `BYWEEKNO`,
//! which nothing here needs yet.
//!
//! One deliberate difference from a calendar: `from` (the RRULE's
//! `DTSTART`) is an occurrence only if the rule matches it.

use jiff::civil::{Date, Weekday};

use crate::clock::{add_days, epoch_day};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

/// One `BYDAY` entry: a weekday, and for `MONTHLY` and `YEARLY` which one
/// of them (`2TU` is the second Tuesday, `-1FR` the last Friday).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByDay {
    pub nth: Option<i8>,
    pub weekday: Weekday,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rrule {
    pub freq: Freq,
    pub interval: u32,
    pub count: Option<u32>,
    pub until: Option<Date>,
    pub by_month: Vec<i8>,
    pub by_month_day: Vec<i8>,
    pub by_day: Vec<ByDay>,
    pub by_set_pos: Vec<i16>,
    pub wkst: Weekday,
}

impl Rrule {
    pub fn parse(s: &str) -> Result<Rrule, String> {
        let body = s.trim();
        let body = match body.get(..6) {
            Some(p) if p.eq_ignore_ascii_case("RRULE:") => &body[6..],
            _ => body,
        };
        let mut freq = None;
        let mut rule = Rrule {
            freq: Freq::Daily,
            interval: 1,
            count: None,
            until: None,
            by_month: vec![],
            by_month_day: vec![],
            by_day: vec![],
            by_set_pos: vec![],
            wkst: Weekday::Monday,
        };
        let mut seen: Vec<String> = vec![];
        for part in body.split(';').filter(|p| !p.trim().is_empty()) {
            let (key, value) = part
                .split_once('=')
                .ok_or_else(|| format!("{part:?} in the RRULE is not KEY=VALUE"))?;
            let key = key.trim().to_ascii_uppercase();
            let value = value.trim();
            if seen.contains(&key) {
                return Err(format!("{key} appears twice in the RRULE"));
            }
            seen.push(key.clone());
            match key.as_str() {
                "FREQ" => {
                    freq = Some(match value.to_ascii_uppercase().as_str() {
                        "DAILY" => Freq::Daily,
                        "WEEKLY" => Freq::Weekly,
                        "MONTHLY" => Freq::Monthly,
                        "YEARLY" => Freq::Yearly,
                        "HOURLY" | "MINUTELY" | "SECONDLY" => {
                            return Err(format!(
                                "FREQ={value}: a rule chooses days; the time of day is `at`"
                            ));
                        }
                        _ => return Err(format!("FREQ={value} is not a frequency")),
                    })
                }
                "INTERVAL" => {
                    rule.interval = value
                        .parse()
                        .ok()
                        .filter(|n| *n >= 1)
                        .ok_or_else(|| format!("INTERVAL={value} is not a positive number"))?
                }
                "COUNT" => {
                    rule.count = Some(
                        value
                            .parse()
                            .ok()
                            .filter(|n| *n >= 1)
                            .ok_or_else(|| format!("COUNT={value} is not a positive number"))?,
                    )
                }
                "UNTIL" => rule.until = Some(parse_until(value)?),
                "BYMONTH" => rule.by_month = list(value, &key, 1, 12)?,
                "BYMONTHDAY" => rule.by_month_day = list(value, &key, -31, 31)?,
                "BYSETPOS" => rule.by_set_pos = list(value, &key, -366, 366)?,
                "BYDAY" => {
                    rule.by_day = value
                        .split(',')
                        .map(|v| parse_by_day(v.trim()))
                        .collect::<Result<_, _>>()?
                }
                "WKST" => {
                    rule.wkst =
                        weekday(value).ok_or_else(|| format!("WKST={value} is not a weekday"))?
                }
                "BYHOUR" | "BYMINUTE" | "BYSECOND" => {
                    return Err(format!(
                        "{key}: the time of day is `at`, not part of the RRULE"
                    ));
                }
                "BYYEARDAY" | "BYWEEKNO" => return Err(format!("{key} is not supported")),
                _ => return Err(format!("{key} is not an RRULE part")),
            }
        }
        rule.freq = freq.ok_or("the RRULE has no FREQ")?;
        if rule.count.is_some() && rule.until.is_some() {
            return Err("COUNT and UNTIL cannot both be given".into());
        }
        let ordinals = rule.by_day.iter().any(|d| d.nth.is_some());
        if ordinals && matches!(rule.freq, Freq::Daily | Freq::Weekly) {
            return Err("a numbered BYDAY (such as 2TU) needs FREQ=MONTHLY or YEARLY".into());
        }
        if rule.freq == Freq::Weekly && !rule.by_month_day.is_empty() {
            return Err("BYMONTHDAY cannot be used with FREQ=WEEKLY".into());
        }
        if !rule.by_set_pos.is_empty()
            && rule.by_month.is_empty()
            && rule.by_month_day.is_empty()
            && rule.by_day.is_empty()
        {
            return Err("BYSETPOS needs another BY part to choose from".into());
        }
        Ok(rule)
    }

    /// Whether the rule leaves a day, a weekday or a count to its start.
    fn needs_start(&self) -> Option<&'static str> {
        if self.interval > 1 {
            return Some("INTERVAL counts from a first occurrence");
        }
        if self.count.is_some() {
            return Some("COUNT counts from a first occurrence");
        }
        let days_given = !self.by_day.is_empty() || !self.by_month_day.is_empty();
        match self.freq {
            Freq::Daily => None,
            Freq::Weekly if self.by_day.is_empty() => {
                Some("WEEKLY without BYDAY takes its weekday from it")
            }
            Freq::Weekly => None,
            Freq::Monthly | Freq::Yearly if !days_given => {
                Some("without BYDAY or BYMONTHDAY the day comes from it")
            }
            Freq::Monthly | Freq::Yearly => None,
        }
    }
}

fn parse_until(v: &str) -> Result<Date, String> {
    let bad = || format!("UNTIL={v} is not a date (expected YYYYMMDD)");
    let d = v
        .get(..8)
        .filter(|d| d.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(bad)?;
    Date::new(
        d[..4].parse().map_err(|_| bad())?,
        d[4..6].parse().map_err(|_| bad())?,
        d[6..].parse().map_err(|_| bad())?,
    )
    .map_err(|_| bad())
}

fn list<T>(v: &str, key: &str, min: i32, max: i32) -> Result<Vec<T>, String>
where
    T: TryFrom<i32>,
{
    v.split(',')
        .map(|n| {
            n.trim()
                .parse::<i32>()
                .ok()
                .filter(|n| *n != 0 && *n >= min && *n <= max)
                .and_then(|n| T::try_from(n).ok())
                .ok_or_else(|| format!("{key}={v}: {n:?} is out of range"))
        })
        .collect()
}

fn weekday(s: &str) -> Option<Weekday> {
    Some(match s.to_ascii_uppercase().as_str() {
        "MO" => Weekday::Monday,
        "TU" => Weekday::Tuesday,
        "WE" => Weekday::Wednesday,
        "TH" => Weekday::Thursday,
        "FR" => Weekday::Friday,
        "SA" => Weekday::Saturday,
        "SU" => Weekday::Sunday,
        _ => return None,
    })
}

fn parse_by_day(s: &str) -> Result<ByDay, String> {
    let bad = || format!("BYDAY entry {s:?} is not a weekday such as TU, 2TU or -1FR");
    if s.len() < 2 {
        return Err(bad());
    }
    let (num, wd) = s.split_at(s.len() - 2);
    let weekday = weekday(wd).ok_or_else(bad)?;
    let nth = if num.is_empty() {
        None
    } else {
        let n: i8 = num.trim_start_matches('+').parse().map_err(|_| bad())?;
        if n == 0 || !(-53..=53).contains(&n) {
            return Err(bad());
        }
        Some(n)
    };
    Ok(ByDay { nth, weekday })
}

/// A rule with the date it counts from: which days it falls on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schedule {
    rule: Rrule,
    from: Option<Date>,
}

impl Schedule {
    pub fn every_day() -> Schedule {
        Schedule {
            rule: Rrule::parse("FREQ=DAILY").unwrap(),
            from: None,
        }
    }

    pub fn new(rule: Rrule, from: Option<Date>) -> Result<Schedule, String> {
        if from.is_none()
            && let Some(why) = rule.needs_start()
        {
            return Err(format!("`from` is required: {why}"));
        }
        Ok(Schedule { rule, from })
    }

    pub fn parse(s: &str, from: Option<Date>) -> Result<Schedule, String> {
        Schedule::new(Rrule::parse(s)?, from)
    }

    /// The days from `a` to `b`, both included, that the schedule falls on.
    pub fn between(&self, a: Date, b: Date) -> Vec<Date> {
        let mut out = vec![];
        if a > b {
            return out;
        }
        let r = &self.rule;
        // With COUNT every occurrence from the first has to be seen to know
        // which ones are left; otherwise the walk can start at `a`.
        let start = match (r.count, self.from) {
            (Some(_), Some(f)) => f,
            (_, Some(f)) => f.max(a),
            (_, None) => a,
        };
        let anchor = self.bucket_index(self.from.unwrap_or(start));
        let mut bucket = self.bucket_start(start);
        let mut counted: u32 = 0;
        'walk: while bucket <= b && r.until.is_none_or(|u| bucket <= u) {
            let i = self.bucket_index(bucket);
            if (i - anchor).rem_euclid(r.interval as i64) == 0 {
                for d in self.in_bucket(bucket) {
                    if self.from.is_some_and(|f| d < f) {
                        continue;
                    }
                    if r.until.is_some_and(|u| d > u) {
                        break 'walk;
                    }
                    if let Some(c) = r.count {
                        if counted == c {
                            break 'walk;
                        }
                        counted += 1;
                    }
                    if d > b {
                        break 'walk;
                    }
                    if d >= a {
                        out.push(d);
                    }
                }
            }
            bucket = self.next_bucket(bucket);
        }
        out
    }

    /// The first day after `d` the schedule falls on, looking up to eight
    /// years ahead -- far enough for a 29th of February.
    pub fn first_after(&self, d: Date) -> Option<Date> {
        let mut lo = add_days(d, 1);
        for span in [40, 400, 2600] {
            let hi = add_days(lo, span);
            if let Some(&x) = self.between(lo, hi).first() {
                return Some(x);
            }
            lo = add_days(hi, 1);
        }
        None
    }

    /// The last day on or before `d` the schedule falls on, looking up to
    /// eight years back.
    pub fn last_on_or_before(&self, d: Date) -> Option<Date> {
        let mut hi = d;
        for span in [40, 400, 2600] {
            let lo = add_days(hi, -span);
            if let Some(&x) = self.between(lo, hi).last() {
                return Some(x);
            }
            hi = add_days(lo, -1);
        }
        None
    }

    fn bucket_start(&self, d: Date) -> Date {
        match self.rule.freq {
            Freq::Daily => d,
            Freq::Weekly => {
                let back = (d.weekday().to_monday_zero_offset()
                    - self.rule.wkst.to_monday_zero_offset())
                .rem_euclid(7);
                add_days(d, -(back as i64))
            }
            Freq::Monthly => d.first_of_month(),
            Freq::Yearly => Date::new(d.year(), 1, 1).unwrap(),
        }
    }

    fn next_bucket(&self, b: Date) -> Date {
        match self.rule.freq {
            Freq::Daily => add_days(b, 1),
            Freq::Weekly => add_days(b, 7),
            Freq::Monthly => add_days(b.last_of_month(), 1),
            Freq::Yearly => Date::new(b.year() + 1, 1, 1).unwrap(),
        }
    }

    /// Consecutive numbers for consecutive buckets, for INTERVAL.
    fn bucket_index(&self, d: Date) -> i64 {
        match self.rule.freq {
            Freq::Daily => epoch_day(d),
            Freq::Weekly => epoch_day(self.bucket_start(d)).div_euclid(7),
            Freq::Monthly => d.year() as i64 * 12 + d.month() as i64 - 1,
            Freq::Yearly => d.year() as i64,
        }
    }

    /// The bucket's occurrences, in order, before COUNT and UNTIL.
    fn in_bucket(&self, b: Date) -> Vec<Date> {
        let r = &self.rule;
        let month_ok = |d: &Date| r.by_month.is_empty() || r.by_month.contains(&d.month());
        let mut days: Vec<Date> = match r.freq {
            Freq::Daily => {
                let ok = month_ok(&b)
                    && (r.by_month_day.is_empty()
                        || r.by_month_day.iter().any(|&k| month_day(b, k) == Some(b)))
                    && (r.by_day.is_empty() || r.by_day.iter().any(|x| x.weekday == b.weekday()));
                if ok { vec![b] } else { vec![] }
            }
            Freq::Weekly => {
                let want = |d: &Date| match (&r.by_day[..], self.from) {
                    ([], Some(f)) => d.weekday() == f.weekday(),
                    (by, _) => by.iter().any(|x| x.weekday == d.weekday()),
                };
                (0..7)
                    .map(|k| add_days(b, k))
                    .filter(|d| want(d) && month_ok(d))
                    .collect()
            }
            Freq::Monthly => {
                if month_ok(&b) {
                    self.in_month(b)
                } else {
                    vec![]
                }
            }
            Freq::Yearly => {
                let months: Vec<Date> = if r.by_month.is_empty() {
                    (1..=12)
                        .map(|m| Date::new(b.year(), m, 1).unwrap())
                        .collect()
                } else {
                    r.by_month
                        .iter()
                        .map(|&m| Date::new(b.year(), m, 1).unwrap())
                        .collect()
                };
                if r.by_month_day.is_empty() && !r.by_day.is_empty() && r.by_month.is_empty() {
                    in_year_by_day(b.year(), &r.by_day)
                } else if r.by_month_day.is_empty() && r.by_day.is_empty() && r.by_month.is_empty()
                {
                    let f = self.from.expect("checked by Schedule::new");
                    Date::new(b.year(), f.month(), f.day())
                        .ok()
                        .into_iter()
                        .collect()
                } else {
                    months.into_iter().flat_map(|m| self.in_month(m)).collect()
                }
            }
        };
        days.sort();
        days.dedup();
        if r.by_set_pos.is_empty() {
            return days;
        }
        let n = days.len() as i64;
        let mut picked: Vec<Date> = r
            .by_set_pos
            .iter()
            .filter_map(|&p| {
                let i = if p > 0 { p as i64 - 1 } else { n + p as i64 };
                (0..n).contains(&i).then(|| days[i as usize])
            })
            .collect();
        picked.sort();
        picked.dedup();
        picked
    }

    /// The days the month starting on `first` contributes.  With neither
    /// BYMONTHDAY nor BYDAY, that is the start's day of the month.
    fn in_month(&self, first: Date) -> Vec<Date> {
        let r = &self.rule;
        if !r.by_month_day.is_empty() {
            let mut days: Vec<Date> = r
                .by_month_day
                .iter()
                .filter_map(|&k| month_day(first, k))
                .collect();
            if !r.by_day.is_empty() {
                days.retain(|d| r.by_day.iter().any(|x| is_by_day_in_month(*d, x)));
            }
            days
        } else if !r.by_day.is_empty() {
            r.by_day
                .iter()
                .flat_map(|x| by_day_in_month(first, x))
                .collect()
        } else {
            let f = self.from.expect("checked by Schedule::new");
            month_day(first, f.day()).into_iter().collect()
        }
    }
}

/// Day `k` of the month `first` is in; negative counts from the end.
fn month_day(first: Date, k: i8) -> Option<Date> {
    let dim = first.days_in_month();
    let day = if k > 0 { k } else { dim + k + 1 };
    (1..=dim)
        .contains(&day)
        .then(|| Date::new(first.year(), first.month(), day).unwrap())
}

fn by_day_in_month(first: Date, x: &ByDay) -> Vec<Date> {
    match x.nth {
        Some(n) => first
            .nth_weekday_of_month(n, x.weekday)
            .ok()
            .into_iter()
            .collect(),
        None => (1..=first.days_in_month())
            .map(|d| Date::new(first.year(), first.month(), d).unwrap())
            .filter(|d| d.weekday() == x.weekday)
            .collect(),
    }
}

fn is_by_day_in_month(d: Date, x: &ByDay) -> bool {
    d.weekday() == x.weekday
        && x.nth
            .is_none_or(|n| d.first_of_month().nth_weekday_of_month(n, x.weekday).ok() == Some(d))
}

fn in_year_by_day(year: i16, by: &[ByDay]) -> Vec<Date> {
    let jan1 = Date::new(year, 1, 1).unwrap();
    let dec31 = Date::new(year, 12, 31).unwrap();
    let all = |wd: Weekday| {
        let skip =
            (wd.to_monday_zero_offset() - jan1.weekday().to_monday_zero_offset()).rem_euclid(7);
        let mut d = add_days(jan1, skip as i64);
        let mut v = vec![];
        while d <= dec31 {
            v.push(d);
            d = add_days(d, 7);
        }
        v
    };
    by.iter()
        .flat_map(|x| {
            let days = all(x.weekday);
            match x.nth {
                None => days,
                Some(n) => {
                    let i = if n > 0 {
                        n as i64 - 1
                    } else {
                        days.len() as i64 + n as i64
                    };
                    usize::try_from(i)
                        .ok()
                        .and_then(|i| days.get(i).copied())
                        .into_iter()
                        .collect()
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn days(rule: &str, from: Option<Date>, a: Date, b: Date) -> Vec<Date> {
        Schedule::parse(rule, from).unwrap().between(a, b)
    }

    #[test]
    fn weekly_on_a_weekday() {
        assert_eq!(
            days(
                "FREQ=WEEKLY;BYDAY=TU",
                None,
                date(2026, 10, 1),
                date(2026, 10, 31)
            ),
            vec![
                date(2026, 10, 6),
                date(2026, 10, 13),
                date(2026, 10, 20),
                date(2026, 10, 27)
            ]
        );
        // The prefix is optional; case is not significant.
        assert_eq!(
            days(
                "RRULE:freq=weekly;byday=tu",
                None,
                date(2026, 10, 1),
                date(2026, 10, 7)
            ),
            vec![date(2026, 10, 6)]
        );
    }

    #[test]
    fn every_other_week_counts_from_the_start() {
        let from = Some(date(2026, 10, 8));
        assert_eq!(
            days(
                "FREQ=WEEKLY;INTERVAL=2;BYDAY=TH",
                from,
                date(2026, 10, 1),
                date(2026, 11, 10)
            ),
            vec![date(2026, 10, 8), date(2026, 10, 22), date(2026, 11, 5)]
        );
        // Starting the walk later does not change which weeks are on.
        assert_eq!(
            days(
                "FREQ=WEEKLY;INTERVAL=2;BYDAY=TH",
                from,
                date(2026, 10, 30),
                date(2026, 11, 30)
            ),
            vec![date(2026, 11, 5), date(2026, 11, 19)]
        );
    }

    #[test]
    fn rfc5545_wkst_example() {
        // RFC 5545 3.8.5.3: the same rule with two week starts.
        let from = Some(date(1997, 8, 5));
        assert_eq!(
            days(
                "FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=MO",
                from,
                date(1997, 1, 1),
                date(1997, 12, 31)
            ),
            vec![
                date(1997, 8, 5),
                date(1997, 8, 10),
                date(1997, 8, 19),
                date(1997, 8, 24)
            ]
        );
        assert_eq!(
            days(
                "FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=SU",
                from,
                date(1997, 1, 1),
                date(1997, 12, 31)
            ),
            vec![
                date(1997, 8, 5),
                date(1997, 8, 17),
                date(1997, 8, 19),
                date(1997, 8, 31)
            ]
        );
    }

    #[test]
    fn rfc5545_first_friday_ten_times() {
        let got = days(
            "FREQ=MONTHLY;COUNT=10;BYDAY=1FR",
            Some(date(1997, 9, 5)),
            date(1997, 1, 1),
            date(1999, 1, 1),
        );
        let want: Vec<Date> = [
            (1997, 9, 5),
            (1997, 10, 3),
            (1997, 11, 7),
            (1997, 12, 5),
            (1998, 1, 2),
            (1998, 2, 6),
            (1998, 3, 6),
            (1998, 4, 3),
            (1998, 5, 1),
            (1998, 6, 5),
        ]
        .into_iter()
        .map(|(y, m, d)| date(y, m, d))
        .collect();
        assert_eq!(got, want);
        // A window that starts after some of them still honours the count.
        assert_eq!(
            days(
                "FREQ=MONTHLY;COUNT=10;BYDAY=1FR",
                Some(date(1997, 9, 5)),
                date(1998, 5, 1),
                date(1999, 1, 1)
            ),
            vec![date(1998, 5, 1), date(1998, 6, 5)]
        );
    }

    #[test]
    fn last_friday_and_last_day() {
        assert_eq!(
            days(
                "FREQ=MONTHLY;BYDAY=-1FR",
                None,
                date(2026, 10, 1),
                date(2026, 12, 31)
            ),
            vec![date(2026, 10, 30), date(2026, 11, 27), date(2026, 12, 25)]
        );
        assert_eq!(
            days(
                "FREQ=MONTHLY;BYMONTHDAY=-1",
                None,
                date(2026, 1, 1),
                date(2026, 3, 31)
            ),
            vec![date(2026, 1, 31), date(2026, 2, 28), date(2026, 3, 31)]
        );
    }

    #[test]
    fn rfc5545_last_work_day_of_the_month() {
        assert_eq!(
            days(
                "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1",
                None,
                date(2026, 1, 1),
                date(2026, 6, 30)
            ),
            vec![
                date(2026, 1, 30),
                date(2026, 2, 27),
                date(2026, 3, 31),
                date(2026, 4, 30),
                date(2026, 5, 29),
                date(2026, 6, 30),
            ]
        );
    }

    #[test]
    fn rfc5545_friday_the_thirteenth() {
        assert_eq!(
            days(
                "FREQ=MONTHLY;BYDAY=FR;BYMONTHDAY=13",
                None,
                date(1997, 9, 2),
                date(2000, 12, 31)
            ),
            vec![
                date(1998, 2, 13),
                date(1998, 3, 13),
                date(1998, 11, 13),
                date(1999, 8, 13),
                date(2000, 10, 13)
            ]
        );
    }

    #[test]
    fn rfc5545_yearly_examples() {
        assert_eq!(
            days(
                "FREQ=YEARLY;BYDAY=20MO",
                Some(date(1997, 5, 19)),
                date(1997, 1, 1),
                date(1999, 12, 31)
            ),
            vec![date(1997, 5, 19), date(1998, 5, 18), date(1999, 5, 17)]
        );
        assert_eq!(
            days(
                "FREQ=YEARLY;BYMONTH=3;BYDAY=TH",
                Some(date(1997, 3, 13)),
                date(1997, 1, 1),
                date(1998, 12, 31)
            ),
            vec![
                date(1997, 3, 13),
                date(1997, 3, 20),
                date(1997, 3, 27),
                date(1998, 3, 5),
                date(1998, 3, 12),
                date(1998, 3, 19),
                date(1998, 3, 26),
            ]
        );
        // A 29th of February comes only in leap years, and is found from afar.
        let leap = Schedule::parse("FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=29", None).unwrap();
        assert_eq!(leap.first_after(date(2024, 3, 1)), Some(date(2028, 2, 29)));
        assert_eq!(
            leap.last_on_or_before(date(2027, 1, 1)),
            Some(date(2024, 2, 29))
        );
    }

    #[test]
    fn the_start_fills_in_what_the_rule_leaves_out() {
        assert_eq!(
            days(
                "FREQ=WEEKLY",
                Some(date(2026, 10, 7)),
                date(2026, 10, 1),
                date(2026, 10, 20)
            ),
            vec![date(2026, 10, 7), date(2026, 10, 14)]
        );
        assert_eq!(
            days(
                "FREQ=MONTHLY",
                Some(date(2026, 1, 31)),
                date(2026, 1, 1),
                date(2026, 4, 30)
            ),
            vec![date(2026, 1, 31), date(2026, 3, 31)]
        );
        assert_eq!(
            days(
                "FREQ=YEARLY",
                Some(date(2026, 10, 7)),
                date(2026, 1, 1),
                date(2028, 12, 31)
            ),
            vec![date(2026, 10, 7), date(2027, 10, 7), date(2028, 10, 7)]
        );
        assert_eq!(
            days(
                "FREQ=DAILY;INTERVAL=3",
                Some(date(2026, 10, 1)),
                date(2026, 10, 5),
                date(2026, 10, 12)
            ),
            vec![date(2026, 10, 7), date(2026, 10, 10)]
        );
    }

    #[test]
    fn until_is_inclusive() {
        assert_eq!(
            days(
                "FREQ=DAILY;UNTIL=20261003",
                None,
                date(2026, 10, 1),
                date(2026, 10, 10)
            ),
            vec![date(2026, 10, 1), date(2026, 10, 2), date(2026, 10, 3)]
        );
        assert_eq!(
            days(
                "FREQ=WEEKLY;BYDAY=MO;UNTIL=20261012T235959Z",
                None,
                date(2026, 10, 1),
                date(2026, 10, 31)
            ),
            vec![date(2026, 10, 5), date(2026, 10, 12)]
        );
    }

    #[test]
    fn refused() {
        for bad in [
            "BYDAY=TU",
            "FREQ=HOURLY",
            "FREQ=WEEKLY;BYHOUR=10",
            "FREQ=WEEKLY;BYDAY=2TU",
            "FREQ=WEEKLY;BYMONTHDAY=1",
            "FREQ=DAILY;COUNT=3;UNTIL=20261231",
            "FREQ=DAILY;BYSETPOS=1",
            "FREQ=DAILY;INTERVAL=0",
            "FREQ=DAILY;FREQ=DAILY",
            "FREQ=YEARLY;BYWEEKNO=1",
            "FREQ=MONTHLY;BYMONTHDAY=0",
            "FREQ=MONTHLY;BYDAY=0MO",
            "FREQ=MONTHLY;BYDAY=XX",
            "FREQ=DAILY;FOO=1",
        ] {
            assert!(Rrule::parse(bad).is_err(), "{bad}");
        }
        for needs_from in [
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=TH",
            "FREQ=DAILY;COUNT=3",
            "FREQ=WEEKLY",
            "FREQ=MONTHLY",
        ] {
            assert!(Schedule::parse(needs_from, None).is_err(), "{needs_from}");
        }
    }
}

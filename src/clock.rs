//! Days, times of day and lengths of time, as the configuration writes them.
//!
//! A *day* here is not midnight to midnight but `day_starts` to `day_starts`,
//! so that a late night still belongs to the day before.  Everything that
//! turns a date and a time of day into an instant goes through [`DayStart`].

use jiff::civil::{Date, DateTime, Time};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

/// Where one day ends and the next begins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DayStart(pub Time);

impl Default for DayStart {
    fn default() -> Self {
        DayStart(Time::midnight())
    }
}

impl DayStart {
    /// The day `ts` belongs to.
    pub fn day_of(&self, ts: Timestamp, tz: &TimeZone) -> Date {
        let dt = tz.to_datetime(ts);
        if dt.time() >= self.0 {
            dt.date()
        } else {
            dt.date().yesterday().expect("a date after the first one")
        }
    }

    /// The instant the clock reads `t` within `day`.  A time earlier than
    /// the day's start is that day's late night, on the next calendar date.
    pub fn at(&self, day: Date, t: Time, tz: &TimeZone) -> Timestamp {
        let date = if t >= self.0 { day } else { next(day) };
        local(date.to_datetime(t), tz)
    }

    /// The first instant of `day`.
    pub fn start(&self, day: Date, tz: &TimeZone) -> Timestamp {
        self.at(day, self.0, tz)
    }

    /// The first instant after `day`.
    pub fn end(&self, day: Date, tz: &TimeZone) -> Timestamp {
        self.start(next(day), tz)
    }
}

/// A local date and time as an instant.  A time a clock change skips over
/// is moved forward by the length of the gap; a time it repeats is the
/// first of the two.
pub fn local(dt: DateTime, tz: &TimeZone) -> Timestamp {
    tz.to_ambiguous_timestamp(dt)
        .compatible()
        .expect("a date and time jiff can represent")
}

/// The first instant after `after` at which the clock reads `t`.
pub fn next_reading(after: Timestamp, t: Time, tz: &TimeZone) -> Timestamp {
    let date = tz.to_datetime(after).date();
    let same_day = local(date.to_datetime(t), tz);
    if same_day > after {
        same_day
    } else {
        local(next(date).to_datetime(t), tz)
    }
}

pub fn next(d: Date) -> Date {
    d.tomorrow().expect("a date before the last one")
}

pub fn prev(d: Date) -> Date {
    d.yesterday().expect("a date after the first one")
}

/// Days since 1970-01-01.  Plain arithmetic, for counting intervals.
pub fn epoch_day(d: Date) -> i64 {
    // Howard Hinnant's days_from_civil.
    let (y, m, dd) = (d.year() as i64, d.month() as i64, d.day() as i64);
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + dd - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn from_epoch_day(n: i64) -> Date {
    let z = n + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    Date::new(y as i16, m as i8, d as i8).expect("a date jiff can represent")
}

pub fn add_days(d: Date, n: i64) -> Date {
    from_epoch_day(epoch_day(d) + n)
}

/// `"10:00"`, `"9:05"` or `"23:59:30"`.
pub fn parse_time(s: &str) -> Result<Time, String> {
    let bad = || format!("{s:?} is not a time of day (expected HH:MM)");
    // The hour may be one digit or two; minutes and seconds are always two.
    let num = |p: &str, widths: &[usize], max: i8| -> Result<i8, String> {
        if !widths.contains(&p.len()) || !p.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        p.parse::<i8>().ok().filter(|n| *n <= max).ok_or_else(bad)
    };
    let parts: Vec<&str> = s.trim().split(':').collect();
    let (h, m, sec) = match parts.as_slice() {
        [h, m] => (num(h, &[1, 2], 23)?, num(m, &[2], 59)?, 0),
        [h, m, sec] => (num(h, &[1, 2], 23)?, num(m, &[2], 59)?, num(sec, &[2], 59)?),
        _ => return Err(bad()),
    };
    Time::new(h, m, sec, 0).map_err(|_| bad())
}

/// `"20m"`, `"2h"`, `"1h30m"`.
pub fn parse_length(s: &str) -> Result<SignedDuration, String> {
    let bad = || format!("{s:?} is not a length of time (expected e.g. 20m, 2h, 1h30m)");
    let mut total: i64 = 0;
    let mut digits = String::new();
    let mut last_unit = 0; // h = 3, m = 2, s = 1 -- units must descend
    let mut seen = false;
    for c in s.trim().chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let (rank, secs) = match c {
            'h' => (3, 3600),
            'm' => (2, 60),
            's' => (1, 1),
            _ => return Err(bad()),
        };
        if digits.is_empty() || (seen && rank >= last_unit) {
            return Err(bad());
        }
        let n: i64 = digits.parse().map_err(|_| bad())?;
        total = total
            .checked_add(n.checked_mul(secs).ok_or_else(bad)?)
            .ok_or_else(bad)?;
        digits.clear();
        last_unit = rank;
        seen = true;
    }
    if !digits.is_empty() || !seen || total == 0 {
        return Err(bad());
    }
    Ok(SignedDuration::from_secs(total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::{date, time};

    fn tz() -> TimeZone {
        TimeZone::fixed(jiff::tz::offset(9))
    }

    #[test]
    fn epoch_days_round_trip() {
        assert_eq!(epoch_day(date(1970, 1, 1)), 0);
        assert_eq!(epoch_day(date(2000, 3, 1)), 11_017);
        for n in [-800_000, -1, 0, 1, 19_000, 20_000, 60_000] {
            assert_eq!(epoch_day(from_epoch_day(n)), n);
        }
        assert_eq!(add_days(date(2024, 2, 28), 1), date(2024, 2, 29));
        assert_eq!(add_days(date(2024, 3, 1), -1), date(2024, 2, 29));
    }

    #[test]
    fn a_late_night_belongs_to_the_day_before() {
        let ds = DayStart(time(4, 0, 0, 0));
        let tz = tz();
        let at = |d: Date, t: Time| local(d.to_datetime(t), &tz);
        assert_eq!(
            ds.day_of(at(date(2026, 10, 8), time(3, 59, 0, 0)), &tz),
            date(2026, 10, 7)
        );
        assert_eq!(
            ds.day_of(at(date(2026, 10, 8), time(4, 0, 0, 0)), &tz),
            date(2026, 10, 8)
        );
        // 02:00 "on" the 7th is the small hours of the 8th.
        assert_eq!(
            ds.at(date(2026, 10, 7), time(2, 0, 0, 0), &tz),
            at(date(2026, 10, 8), time(2, 0, 0, 0))
        );
        assert_eq!(
            ds.end(date(2026, 10, 7), &tz),
            at(date(2026, 10, 8), time(4, 0, 0, 0))
        );
    }

    #[test]
    fn next_reading_is_strictly_after() {
        let tz = tz();
        let t0 = local(date(2026, 10, 8).to_datetime(time(10, 0, 0, 0)), &tz);
        assert_eq!(
            next_reading(t0, time(10, 20, 0, 0), &tz),
            local(date(2026, 10, 8).to_datetime(time(10, 20, 0, 0)), &tz)
        );
        assert_eq!(
            next_reading(t0, time(10, 0, 0, 0), &tz),
            local(date(2026, 10, 9).to_datetime(time(10, 0, 0, 0)), &tz)
        );
        assert_eq!(
            next_reading(t0, time(9, 0, 0, 0), &tz),
            local(date(2026, 10, 9).to_datetime(time(9, 0, 0, 0)), &tz)
        );
    }

    #[test]
    fn a_skipped_time_moves_forward_by_the_gap() {
        let tz = TimeZone::get("America/New_York").unwrap();
        // 2026-03-08 02:30 does not exist there; it is read as 03:30 EDT.
        let ts = local(date(2026, 3, 8).to_datetime(time(2, 30, 0, 0)), &tz);
        assert_eq!(
            tz.to_datetime(ts),
            date(2026, 3, 8).to_datetime(time(3, 30, 0, 0))
        );
    }

    #[test]
    fn times_of_day() {
        assert_eq!(parse_time("10:00"), Ok(time(10, 0, 0, 0)));
        assert_eq!(parse_time("9:05"), Ok(time(9, 5, 0, 0)));
        assert_eq!(parse_time("23:59:30"), Ok(time(23, 59, 30, 0)));
        for bad in [
            "", "10", "24:00", "10:60", "10:5", "1000", "ab:cd", "10:00:61", "-1:00",
        ] {
            assert!(parse_time(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn lengths() {
        assert_eq!(parse_length("20m"), Ok(SignedDuration::from_mins(20)));
        assert_eq!(parse_length("2h"), Ok(SignedDuration::from_hours(2)));
        assert_eq!(parse_length("1h30m"), Ok(SignedDuration::from_mins(90)));
        assert_eq!(parse_length("90s"), Ok(SignedDuration::from_secs(90)));
        for bad in ["", "20", "m", "0m", "30m1h", "2h2h", "1d", "-5m", "1.5h"] {
            assert!(parse_length(bad).is_err(), "{bad:?}");
        }
    }
}

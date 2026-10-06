//! Days and stretches of time as the commands take them: `2026-10-15`,
//! `today`, `tomorrow`, `thu`, `13:00-15:00`.

use jiff::civil::{Date, Time, Weekday};

use crate::clock::{add_days, parse_time};

/// A day, relative to `today` (the day by `day_starts`).  A weekday name
/// is the nearest one, today included.
pub fn parse_day(s: &str, today: Date) -> Result<Date, String> {
    let lower = s.trim().to_ascii_lowercase();
    match lower.as_str() {
        "today" => return Ok(today),
        "tomorrow" => return Ok(add_days(today, 1)),
        _ => {}
    }
    if let Some(wd) = weekday(&lower) {
        let ahead =
            (wd.to_monday_zero_offset() - today.weekday().to_monday_zero_offset()).rem_euclid(7);
        return Ok(add_days(today, ahead as i64));
    }
    s.trim().parse::<Date>().map_err(|_| {
        format!("{s:?} is not a day (expected 2026-10-15, today, tomorrow or a weekday)")
    })
}

fn weekday(s: &str) -> Option<Weekday> {
    const NAMES: [(&str, Weekday); 7] = [
        ("monday", Weekday::Monday),
        ("tuesday", Weekday::Tuesday),
        ("wednesday", Weekday::Wednesday),
        ("thursday", Weekday::Thursday),
        ("friday", Weekday::Friday),
        ("saturday", Weekday::Saturday),
        ("sunday", Weekday::Sunday),
    ];
    if s.len() < 3 {
        return None;
    }
    NAMES
        .iter()
        .find(|(name, _)| name.starts_with(s))
        .map(|(_, wd)| *wd)
}

/// `13:00-15:00`.  An end earlier than the start is on the next day.
pub fn parse_range(s: &str) -> Result<(Time, Time), String> {
    let (a, b) = s
        .split_once('-')
        .ok_or_else(|| format!("{s:?} is not a stretch of time (expected 13:00-15:00)"))?;
    let (from, to) = (parse_time(a)?, parse_time(b)?);
    if from == to {
        return Err(format!("{s:?} starts and ends at the same time"));
    }
    Ok((from, to))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::{date, time};

    const TUE: Date = date(2026, 10, 13);

    #[test]
    fn days() {
        assert_eq!(parse_day("today", TUE), Ok(TUE));
        assert_eq!(parse_day("Tomorrow", TUE), Ok(date(2026, 10, 14)));
        assert_eq!(parse_day("tue", TUE), Ok(TUE), "today, if today is the day");
        assert_eq!(parse_day("mon", TUE), Ok(date(2026, 10, 19)));
        assert_eq!(parse_day("thursday", TUE), Ok(date(2026, 10, 15)));
        assert_eq!(parse_day("thurs", TUE), Ok(date(2026, 10, 15)));
        assert_eq!(parse_day("2026-12-24", TUE), Ok(date(2026, 12, 24)));
        for bad in ["", "th", "someday", "2026-13-01", "12/24"] {
            assert!(parse_day(bad, TUE).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn ranges() {
        assert_eq!(
            parse_range("13:00-15:00"),
            Ok((time(13, 0, 0, 0), time(15, 0, 0, 0)))
        );
        assert_eq!(
            parse_range("23:00-1:00"),
            Ok((time(23, 0, 0, 0), time(1, 0, 0, 0)))
        );
        for bad in ["13:00", "13:00-13:00", "1pm-3pm", "-15:00"] {
            assert!(parse_range(bad).is_err(), "{bad:?}");
        }
    }
}

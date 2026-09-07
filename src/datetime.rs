//! Just enough of `datetime64[ns]` for the aggregation step.
//!
//! A timestamp is nanoseconds since the Unix epoch, which is exactly what
//! pandas stores — and the fixtures need that resolution: `DATE-OBS` values
//! like `2026-09-04T21:09:26.4499226` carry seven fractional digits.
//!
//! `pd.to_datetime(errors='coerce')` accepts a great deal more than this
//! (dozens of formats, timezones, epoch integers). What is implemented here is
//! the ISO-8601 subset every observation timestamp in this pipeline actually
//! uses; anything else is `NaT`, which is also what a coerced parse failure
//! gives. Widen it when a real dataset needs it, not on speculation — a
//! silently *wrong* parse would be far worse than a NaT, since NaT rows sort
//! to the end and are visible.

/// Nanoseconds since the Unix epoch. `None` is `NaT`.
pub type Timestamp = i64;

const NS_PER_SEC: i64 = 1_000_000_000;
const NS_PER_DAY: i64 = 86_400 * NS_PER_SEC;

/// `pd.to_datetime(text, errors='coerce')` for the ISO-8601 forms this
/// pipeline sees: `YYYY-MM-DD`, optionally `T` or space, `HH:MM[:SS[.frac]]`.
///
/// A trailing timezone is deliberately *not* accepted: pandas would return a
/// tz-aware timestamp (or, mixed with naive ones, an object column), and
/// silently dropping the offset would shift the observation into the wrong
/// night.
pub fn parse(text: &str) -> Option<Timestamp> {
    let s = text.trim();
    if s.is_empty() {
        return None;
    }
    let (date, time) = match s.split_once(['T', ' ']) {
        Some((d, t)) => (d, t),
        None => (s, ""),
    };

    let mut parts = date.split('-');
    let y: i64 = parse_int(parts.next()?)?;
    let mo: i64 = parse_int(parts.next()?)?;
    let d: i64 = parse_int(parts.next()?)?;
    if parts.next().is_some() || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }

    let mut ns = days_from_civil(y, mo as u32, d as u32) * NS_PER_DAY;
    if time.is_empty() {
        return Some(ns);
    }

    let mut tp = time.split(':');
    let h: i64 = parse_int(tp.next()?)?;
    let mi: i64 = match tp.next() {
        Some(v) => parse_int(v)?,
        None => return None, // 'YYYY-MM-DDTHH' alone is not a form we accept
    };
    let (sec, frac_ns) = match tp.next() {
        None => (0, 0),
        Some(sec_text) => match sec_text.split_once('.') {
            None => (parse_int(sec_text)?, 0),
            Some((whole, frac)) => {
                if frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                // Nine digits of nanoseconds; pandas truncates beyond that.
                let padded: String = frac.chars().chain(std::iter::repeat('0')).take(9).collect();
                (parse_int(whole)?, padded.parse::<i64>().ok()?)
            }
        },
    };
    if tp.next().is_some() || h > 23 || mi > 59 || sec > 60 {
        return None;
    }

    ns += ((h * 60 + mi) * 60 + sec) * NS_PER_SEC + frac_ns;
    Some(ns)
}

fn parse_int(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Days since 1970-01-01 from a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = ((m + 9) % 12) as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The inverse: a civil date from days since the epoch.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Whole days since the epoch, flooring — so a timestamp before 1970 still
/// lands on the right calendar day.
pub fn days(ts: Timestamp) -> i64 {
    ts.div_euclid(NS_PER_DAY)
}

/// Nanoseconds since local midnight, always non-negative.
pub fn time_of_day_ns(ts: Timestamp) -> i64 {
    ts.rem_euclid(NS_PER_DAY)
}

/// `Timestamp.date()` rendered as `str(datetime.date)` / `strftime('%Y-%m-%d')`
/// — the two agree, which is why the aggregated frame's `session_date` and
/// `start_date` look alike despite coming from different calls.
pub fn date_string(ts: Timestamp) -> String {
    let (y, m, d) = civil_from_days(days(ts));
    format!("{y:04}-{m:02}-{d:02}")
}

/// `ts - timedelta(days=1)`.
pub fn minus_one_day(ts: Timestamp) -> Timestamp {
    ts - NS_PER_DAY
}

/// Noon, as `pd.Timestamp("12:00:00").time()`.
pub const NOON_NS: i64 = 12 * 3600 * NS_PER_SEC;

/// Five hours, the session-gap threshold.
pub const SESSION_GAP_NS: i64 = 5 * 3600 * NS_PER_SEC;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seven_fractional_digits_survive() {
        // The sh2101 fixture's DATE-OBS resolution.
        let ts = parse("2026-09-04T21:09:26.4499226").unwrap();
        assert_eq!(time_of_day_ns(ts) % NS_PER_SEC, 449_922_600);
        assert_eq!(date_string(ts), "2026-09-04");
    }

    #[test]
    fn a_bare_date_is_midnight() {
        let ts = parse("2023-01-01").unwrap();
        assert_eq!(time_of_day_ns(ts), 0);
        assert_eq!(date_string(ts), "2023-01-01");
    }

    #[test]
    fn space_and_t_separators_agree() {
        assert_eq!(parse("2023-07-06 01:08:40.138"), parse("2023-07-06T01:08:40.138"));
    }

    #[test]
    fn unparseable_text_is_nat_not_a_guess() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("nan"), None);
        assert_eq!(parse("06/07/2023"), None);
        // A timezone is refused rather than silently dropped.
        assert_eq!(parse("2023-07-06T01:08:40Z"), None);
        assert_eq!(parse("2023-07-06T01:08:40+01:00"), None);
    }

    #[test]
    fn civil_dates_round_trip_across_leap_years_and_the_epoch() {
        for (y, m, d) in [
            (1970, 1, 1),
            (2000, 2, 29),
            (2023, 12, 31),
            (2026, 9, 4),
            (1969, 12, 31),
        ] {
            let z = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(z), (y, m, d), "{y}-{m}-{d}");
        }
    }

    #[test]
    fn day_arithmetic_floors_before_the_epoch() {
        let ts = parse("1969-12-31T01:00:00").unwrap();
        assert!(ts < 0);
        assert_eq!(date_string(ts), "1969-12-31");
        assert_eq!(date_string(minus_one_day(ts)), "1969-12-30");
    }
}

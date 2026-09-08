//! `engine/reports.py` — the human-readable session summary.
//!
//! The body of the summary is plain `str.format` padding, which ports
//! directly. What does not port directly is the arithmetic behind the
//! numbers, because three different reductions appear within a few lines of
//! each other and they do not agree in the last bit:
//!
//! - the per-row tables use `groupby(...).agg({'x': 'mean'})` — Kahan
//!   compensated, [`kahan_mean`];
//! - `get_observation_period` uses a bare `Series.mean()` — numpy's
//!   **pairwise** summation, [`crate::steps::pairwise_mean`]. This is the one
//!   call site PORT_PLAN.md hazard 4 warned would eventually appear;
//! - `.iloc[0]` reads row 0 positionally and does **not** skip nulls, unlike
//!   the `agg('first')` that produced most of these columns (hazard 6).
//!
//! `.iloc[0]` also reads two different frames within one site section:
//! latitude/longitude/bortle/sqm come from `site_group` (calibration rows
//! included), while the equipment names come from the LIGHT subset.
//!
//! And every `groupby` in this module runs with the default `dropna=True`
//! and **no** key fill — unlike `AggregationStep`, which fills every key
//! first. A null `filter`, `gain_match` or `exposure` silently removes that
//! row from its table (hazard 2).

use std::collections::HashSet;

use crate::constants as col;
use crate::constants::image_type as it;
use crate::steps::{astype_str, kahan_mean, kahan_sum, pairwise_mean, to_numeric};
use crate::table::{Cell, Table};

/// `f"{x:.Nf}"`. Rust prints `NaN` where Python prints `nan`; every other
/// spelling (`inf`, `-inf`, `-0.00`) already agrees.
fn fmt_f(x: f64, n: usize) -> String {
    if x.is_nan() {
        "nan".to_string()
    } else {
        format!("{:.*}", n, x)
    }
}

/// `"{:<width}".format(s)` — Python pads by code points and so does Rust.
fn ljust(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - n))
    }
}

fn rjust(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n >= width {
        s.to_string()
    } else {
        format!("{}{s}", " ".repeat(width - n))
    }
}

/// CPython's `float.__divmod__`, which is not `(x / y).floor()` and `x % y`.
/// Python's `%` is floor-modulo; Rust's is truncated. They agree for the
/// non-negative durations this module handles, but the exposure total is a
/// user-supplied product and a negative one should degrade the way Python's
/// does rather than a different way.
fn py_divmod(vx: f64, wx: f64) -> (f64, f64) {
    let mut m = vx % wx; // C fmod, which is what Rust's % is for f64
    let mut d = (vx - m) / wx;
    if m != 0.0 {
        if (wx < 0.0) != (m < 0.0) {
            m += wx;
            d -= 1.0;
        }
    } else {
        m = 0.0f64.copysign(wx);
    }
    let floordiv = if d != 0.0 {
        let f = d.floor();
        if d - f > 0.5 {
            f + 1.0
        } else {
            f
        }
    } else {
        0.0f64.copysign(vx / wx)
    };
    (floordiv, m)
}

/// `seconds_to_hms`. The aligned form is the one used inside the ASCII
/// tables; the bare form ends each per-target and per-type total.
fn seconds_to_hms(seconds: f64, aligned: bool) -> String {
    if !seconds.is_finite() {
        // `int(nan)` raises ValueError and `int(inf)` OverflowError; the
        // Python catches both and returns the *unaligned* zero string
        // regardless of `aligned`, so the fallback is deliberately not
        // padded here either.
        return "0 hrs 0 mins 0.00 secs".to_string();
    }
    let hours = py_divmod(seconds, 3600.0).0 as i64;
    let rem = py_divmod(seconds, 3600.0).1;
    let minutes = py_divmod(rem, 60.0).0 as i64;
    let secs = py_divmod(seconds, 60.0).1;
    if aligned {
        format!(
            "{:>6} hrs {:>6} mins {:>6} secs",
            hours,
            minutes,
            rjust(&fmt_f(secs, 2), 6)
        )
    } else {
        format!("{hours} hrs {minutes} mins {} secs", fmt_f(secs, 2))
    }
}

/// A cell is missing for grouping and `isin` purposes when it is NA — a null
/// slot or a float NaN, which `Rule::Mean` and `Rule::First` can both
/// produce.
fn is_na(c: &Cell) -> bool {
    matches!(c, Cell::Null) || matches!(c, Cell::Float(f) if f.is_nan())
}

/// A hashable stand-in for a cell. Every NA collapses to one key, which is
/// also how `Series.isin` behaves: `Series([nan]).isin([nan])` is `True`.
fn key_of(c: &Cell) -> String {
    if is_na(c) {
        "\u{0}NA".to_string()
    } else {
        format!("{}\u{1}{}", tag(c), astype_str(c))
    }
}

fn tag(c: &Cell) -> char {
    match c {
        Cell::Int(_) => 'i',
        Cell::Float(_) => 'f',
        Cell::Bool(_) => 'b',
        Cell::Str(_) => 's',
        Cell::Null => 'n',
    }
}

/// `df.groupby(keys, observed=True)` over a row subset: groups sorted by the
/// key tuple, rows in their original order inside each group, and **rows
/// with an NA in any key dropped**.
fn group_rows(t: &Table, keys: &[&str], rows: &[usize]) -> Vec<(Vec<Cell>, Vec<usize>)> {
    let cols: Vec<&crate::table::Column> = keys.iter().filter_map(|k| t.column(k)).collect();
    if cols.len() != keys.len() {
        // `groupby` on a missing column raises KeyError in Python. Every key
        // this module groups on is produced unconditionally by
        // `aggregate.rs`'s rules, so the branch is unreachable; an empty
        // result at least degrades to an omitted table rather than a panic.
        return Vec::new();
    }
    let mut keyed: Vec<(Vec<Cell>, usize)> = rows
        .iter()
        .filter(|&&i| !cols.iter().any(|c| is_na(&c.cells[i])))
        .map(|&i| (cols.iter().map(|c| c.cells[i].clone()).collect(), i))
        .collect();
    keyed.sort_by(|a, b| crate::steps::aggregate::compare_keys(&a.0, &b.0));

    let mut groups: Vec<(Vec<Cell>, Vec<usize>)> = Vec::new();
    for (key, row) in keyed {
        match groups.last_mut() {
            Some((k, r)) if *k == key => r.push(row),
            _ => groups.push((key, vec![row])),
        }
    }
    groups
}

/// `series.iloc[0]` over a row subset — positional, null or not.
fn iloc0(t: &Table, name: &str, rows: &[usize]) -> Option<Cell> {
    let c = t.column(name)?;
    rows.first().map(|&i| c.cells[i].clone())
}

fn numeric_at(t: &Table, name: &str, rows: &[usize]) -> Vec<f64> {
    match t.column(name) {
        Some(c) => rows.iter().filter_map(|&i| to_numeric(&c.cells[i])).collect(),
        None => Vec::new(),
    }
}

/// `int(group[NUMBER].sum())` — the sum first, the truncation **once**, at
/// the end.
///
/// Stage 7 hardens `number` to int64 unconditionally, so this only ever adds
/// whole numbers today. Truncating per element would be indistinguishable
/// until it wasn't: `[1.5, 1.5]` sums to `3` in Python and would give `2`
/// here. `aggregate.rs`'s `Rule::Sum` guards the same unreachable case for
/// the same reason, and the two sum sites should not disagree about it.
fn int_sum(t: &Table, name: &str, rows: &[usize]) -> i64 {
    kahan_sum(numeric_at(t, name, rows)) as i64
}

/// `Series.mean()` — pairwise, over the NA-as-zero values of the *whole*
/// subset. Dropping the missing rows first would change the summation tree
/// and with it the last bit, so they are kept as zeros and only excluded
/// from the divisor.
fn series_mean(t: &Table, name: &str, rows: &[usize]) -> f64 {
    let Some(c) = t.column(name) else {
        return f64::NAN;
    };
    let mut count = 0usize;
    let values: Vec<f64> = rows
        .iter()
        .map(|&i| match to_numeric(&c.cells[i]) {
            Some(v) => {
                count += 1;
                v
            }
            None => 0.0,
        })
        .collect();
    pairwise_mean(&values, count)
}

/// `groupby(...).agg({col: 'mean'})` — Kahan, and NaN over an empty group.
fn agg_mean(t: &Table, name: &str, rows: &[usize]) -> f64 {
    let v = numeric_at(t, name, rows);
    if v.is_empty() {
        f64::NAN
    } else {
        kahan_mean(v)
    }
}

/// `agg('first')` — the first non-null value, not `.iloc[0]`.
fn agg_first(t: &Table, name: &str, rows: &[usize]) -> Cell {
    match t.column(name) {
        Some(c) => rows
            .iter()
            .map(|&i| &c.cells[i])
            .find(|x| !x.is_null())
            .cloned()
            .unwrap_or(Cell::Null),
        None => Cell::Null,
    }
}

/// `get_target_details`.
fn get_target_details(t: &Table, lights: &[usize]) -> String {
    if lights.is_empty() {
        return " Target: No target data".to_string();
    }
    let Some(c) = t.column(col::TARGET) else {
        return " Target: Unknown".to_string();
    };
    // `.dropna().unique()` — first-occurrence order.
    let mut seen: HashSet<String> = HashSet::new();
    let mut unique: Vec<String> = Vec::new();
    for &i in lights {
        let cell = &c.cells[i];
        if is_na(cell) {
            continue;
        }
        let s = astype_str(cell);
        if seen.insert(s.clone()) {
            unique.push(s);
        }
    }
    let panels: Vec<&String> = unique.iter().filter(|s| s.contains("Panel")).collect();
    if let Some(first) = panels.first() {
        let base = first.split("Panel").next().unwrap_or("").trim();
        return format!(" Target: {base} {} Panel Mosaic", panels.len());
    }
    match unique.first() {
        Some(t) => format!(" Target: {t}"),
        None => " Target: Unknown".to_string(),
    }
}

/// `get_equipment_used`. Hardware names come from the LIGHT subset's row 0;
/// the software list is the union of the LIGHT subset's and the *whole*
/// frame's distinct `swcreate` values.
fn get_equipment_used(t: &Table, lights: &[usize]) -> String {
    let mut s = vec!["\nEquipment used:".to_string()];
    let items: [(&str, &str); 5] = [
        ("Telescope", col::TELESCOPE),
        ("Camera", col::CAMERA),
        ("Filterwheel", col::FILTER_WHEEL),
        ("Focuser", col::FOCUSER),
        ("Rotator", col::ROTATOR_NAME),
    ];
    for (label, name) in items {
        if !t.has_column(name) {
            continue;
        }
        let Some(val) = iloc0(t, name, lights) else {
            continue;
        };
        if is_na(&val) {
            continue;
        }
        let text = astype_str(&val);
        let lowered = text.to_lowercase();
        if matches!(lowered.as_str(), "none" | "nan" | "") {
            continue;
        }
        s.push(format!("\t{}: {text}", ljust(label, 20)));
    }

    let mut sw: HashSet<String> = HashSet::new();
    if let Some(c) = t.column(col::SWCREATE) {
        for &i in lights {
            if !is_na(&c.cells[i]) {
                sw.insert(astype_str(&c.cells[i]));
            }
        }
        for cell in &c.cells {
            if !is_na(cell) {
                sw.insert(astype_str(cell));
            }
        }
    }
    if !sw.is_empty() {
        // `sorted(..., reverse=True)`: descending by code point, which for
        // UTF-8 is Rust's byte order.
        let mut list: Vec<String> = sw.into_iter().collect();
        list.sort();
        list.reverse();
        let mut iter = list.into_iter();
        let first = iter.next().expect("non-empty");
        s.push(format!("\t{}: {first}", ljust("Capture software", 20)));
        for item in iter {
            s.push(format!("\t{}: {item}", ljust("", 20)));
        }
    }
    s.join("\n") + "\n"
}

/// `get_observation_period`.
fn get_observation_period(t: &Table, lights: &[usize]) -> String {
    let mut s = vec!["\nObservation period:".to_string()];
    let text = |name: &str, default: &str| -> String {
        if t.has_column(name) {
            iloc0(t, name, lights).map(|c| astype_str(&c)).unwrap_or_else(|| default.to_string())
        } else {
            default.to_string()
        }
    };
    let int_of = |name: &str| -> i64 {
        if t.has_column(name) {
            iloc0(t, name, lights)
                .and_then(|c| to_numeric(&c))
                .map(|v| v as i64)
                .unwrap_or(0)
        } else {
            0
        }
    };
    let row = |label: &str, value: &str| format!("\t{}: {value}", ljust(label, 25));

    s.push(row("Start date", &text("start_date", "N/A")));
    s.push(row("End date", &text("end_date", "N/A")));
    s.push(row("Days", &int_of("num_days").to_string()));
    s.push(row("Observation sessions", &int_of("sessions").to_string()));

    if t.has_column("temp_min") {
        let st = temp_stats(t, lights);
        s.push(row("Min temperature", &format!("{}\u{b0}C", fmt_f(st.min, 1))));
        s.push(row("Max temperature", &format!("{}\u{b0}C", fmt_f(st.max, 1))));
        s.push(row("Mean temperature", &format!("{}\u{b0}C", fmt_f(st.mean, 1))));
    }
    s.join("\n") + "\n"
}

/// The three temperature statistics behind the "Observation period" block,
/// before `:.1f` throws away fifteen of their digits.
///
/// A green summary diff proves almost nothing about `mean`: one decimal place
/// hides every summation-order difference there is. `--dump-report-stats`
/// emits these at full precision so `parity/check_reports.py` can compare the
/// bits against pandas directly.
pub struct TempStats {
    pub count: usize,
    pub min: f64,
    pub max: f64,
    pub mean: f64,
}

fn temp_stats(t: &Table, lights: &[usize]) -> TempStats {
    TempStats {
        count: lights.len(),
        // `Series.min()`/`.max()` are nan-skipping and order-independent.
        min: numeric_at(t, "temp_min", lights)
            .into_iter()
            .fold(f64::NAN, f64::min),
        max: numeric_at(t, "temp_max", lights)
            .into_iter()
            .fold(f64::NAN, f64::max),
        // The one bare `Series.mean()` in the codebase: numpy pairwise
        // summation over the NA-as-zero values, divided by the non-NA count
        // -- *not* the Kahan sum every groupby mean uses.
        mean: series_mean(t, col::TEMPERATURE, lights),
    }
}

/// `(site, stats)` for every site the report would emit a section for, in the
/// report's own order. Exists only for the parity harness.
pub fn report_temp_stats(df: &Table) -> Vec<(String, TempStats)> {
    let all_rows: Vec<usize> = (0..df.n_rows).collect();
    let mut out = Vec::new();
    for (site_key, site_rows) in group_rows(df, &[col::SITE_NAME], &all_rows) {
        let lights = light_rows(df, &site_rows);
        if lights.is_empty() {
            continue;
        }
        out.push((astype_str(&site_key[0]), temp_stats(df, &lights)));
    }
    out
}

/// `site_group[site_group[IMAGE_TYPE] == 'LIGHT']`.
fn light_rows(df: &Table, site_rows: &[usize]) -> Vec<usize> {
    match df.column(col::IMAGE_TYPE) {
        Some(c) => site_rows
            .iter()
            .copied()
            .filter(|&i| matches!(&c.cells[i], Cell::Str(s) if s == it::LIGHT))
            .collect(),
        None => Vec::new(),
    }
}

const LIGHT_HEADER_WIDTHS: [usize; 9] = [8, 8, 8, 12, 12, 12, 12, 15, 15];
const CAL_HEADER_WIDTHS: [usize; 6] = [10, 8, 10, 15, 12, 15];

/// `" {:<w1} {:<w2} ...".format(...)` — one leading space, then each field
/// left-justified and separated by a single space. Note that the last field
/// is padded too, so every header line and every row of a table that fits its
/// widths carries trailing spaces. They are part of the reference bytes.
fn row_fmt(widths: &[usize], fields: &[String]) -> String {
    let mut out = String::new();
    for (w, f) in widths.iter().zip(fields) {
        out.push(' ');
        out.push_str(&ljust(f, *w));
    }
    out
}

/// `format_image_type_table`. Returns the table text and the total exposure
/// seconds it accounts for.
fn format_image_type_table(
    t: &Table,
    rows: &[usize],
    imagetype: &str,
    light_filters: Option<&HashSet<String>>,
    light_gains: Option<&HashSet<String>>,
) -> (String, f64) {
    let mut lines: Vec<String> = Vec::new();
    let mut total_exposure = 0.0f64;

    // The caller already scoped `rows` to this category's image types, so no
    // further type filtering happens here (A13 upstream: re-filtering on the
    // base type alone dropped groups that had only their MASTER variant).
    let mut group: Vec<usize> = rows.to_vec();

    if let Some(filters) = light_filters {
        if imagetype.to_uppercase().contains("FLAT") {
            if let Some(c) = t.column(col::FILTER_NAME) {
                group.retain(|&i| filters.contains(&astype_str(&c.cells[i]).to_lowercase()));
            }
        }
    }
    if let Some(gains) = light_gains {
        if imagetype != it::LIGHT {
            if let Some(c) = t.column(col::GAIN_MATCH) {
                group.retain(|&i| gains.contains(&key_of(&c.cells[i])));
            }
        }
    }
    if group.is_empty() {
        return (String::new(), 0.0);
    }

    let table_group_keys = [col::FILTER_NAME, col::GAIN_MATCH, col::DURATION];

    if imagetype == it::LIGHT {
        lines.push(format!("\n {imagetype}S:"));
        for (key, t_rows) in group_rows(t, &[col::TARGET], &group) {
            let target = astype_str(&key[0]);
            lines.push(format!(" Target: {target}\n"));
            lines.push(row_fmt(
                &LIGHT_HEADER_WIDTHS,
                &[
                    "Filter", "Frames", "Gain", "Egain", "Mean FWHM", "Sensor Temp", "Mean Temp",
                    "Exposure", "Total Exposure",
                ]
                .map(String::from),
            ));

            let mut t_exposure_target = 0.0f64;
            for (gkey, grows) in group_rows(t, &table_group_keys, &t_rows) {
                let number = int_sum(t, col::NUMBER, &grows);
                let duration = to_numeric(&gkey[2]).unwrap_or(f64::NAN);
                let row_total = number as f64 * duration;
                t_exposure_target += row_total;

                let gain = agg_first(t, col::GAIN, &grows);
                let gain_str = if is_na(&gain) {
                    "N/A".to_string()
                } else {
                    let v = to_numeric(&gain).unwrap_or(f64::NAN);
                    (crate::numeric::python_round(v, 0) as i64).to_string()
                };
                let egain_str = format!("{} e/ADU", fmt_f(agg_mean(t, col::EGAIN, &grows), 2));

                lines.push(row_fmt(
                    &LIGHT_HEADER_WIDTHS,
                    &[
                        astype_str(&gkey[0]),
                        number.to_string(),
                        gain_str,
                        egain_str,
                        format!("{} arcsec", fmt_f(agg_mean(t, col::MEAN_FWHM, &grows), 2)),
                        format!(
                            "{}\u{b0}C",
                            fmt_f(agg_mean(t, col::SENSOR_COOLING, &grows), 1)
                        ),
                        format!(
                            "{}\u{b0}C",
                            fmt_f(agg_mean(t, col::TEMPERATURE, &grows), 1)
                        ),
                        format!("{} secs", fmt_f(duration, 2)),
                        seconds_to_hms(row_total, true),
                    ],
                ));
            }
            lines.push(format!(
                "\n Exposure time for {target}: {}\n",
                seconds_to_hms(t_exposure_target, false)
            ));
            total_exposure += t_exposure_target;
        }
    } else {
        let upper = imagetype.to_uppercase();
        // The label reflects what this table actually contains, not a blind
        // lookup keyed on the category's base type (Python v2.1.1 <= 2.1.1
        // hazard, fixed upstream in v2.1.2: every calibration section used
        // to read MASTERxxx unconditionally, including a session built
        // entirely from raw, uncalibrated frames with no master anywhere).
        // `imagetype` is always the category's base (raw) type -- every
        // `order` tuple in `generate_full_summary` is (raw, MASTER_raw) --
        // so pluralising it needs the same BIAS exception the old lookup
        // encoded: "BIAS" takes no extra S, every other type does.
        let plain_label = if upper == "BIAS" {
            upper.clone()
        } else {
            format!("{upper}S")
        };
        let has_master = t
            .column(col::IMAGE_TYPE)
            .map(|c| {
                group
                    .iter()
                    .any(|&i| astype_str(&c.cells[i]).to_uppercase().starts_with("MASTER"))
            })
            .unwrap_or(false);
        // A genuinely mixed table (a master covering one gain, raw frames
        // surviving for another the master doesn't cover) favours MASTER --
        // the safer thing to over-claim toward when the table isn't uniform.
        let display_label = if has_master {
            format!("MASTER{plain_label}")
        } else {
            plain_label
        };
        lines.push(format!("\n {display_label}:\n"));
        lines.push(row_fmt(
            &CAL_HEADER_WIDTHS,
            &["Filter", "Frames", "Gain", "Egain", "Exposure", "Total Exposure"].map(String::from),
        ));

        // Darks and bias are filter-independent, and calibration.py's own
        // candidate matching already reflects that; grouping them by filter
        // fragments one logical set across a filter change. Flats (and
        // darkflats, which match flats) do constrain on filter.
        let filter_matters = upper.contains("FLAT");
        let cal_keys: Vec<&str> = if filter_matters {
            vec![col::FILTER_NAME, col::GAIN_MATCH, col::DURATION]
        } else {
            vec![col::GAIN_MATCH, col::DURATION]
        };

        for (gkey, grows) in group_rows(t, &cal_keys, &group) {
            let number = int_sum(t, col::NUMBER, &grows);
            let duration = to_numeric(gkey.last().expect("duration is the last key"))
                .unwrap_or(f64::NAN);
            let row_total = number as f64 * duration;
            total_exposure += row_total;

            let gain = agg_first(t, col::GAIN, &grows);
            let gain_str = if is_na(&gain) {
                "N/A".to_string()
            } else {
                let v = to_numeric(&gain).unwrap_or(f64::NAN);
                (crate::numeric::python_round(v, 0) as i64).to_string()
            };
            let egain_str = format!("{} e/ADU", fmt_f(agg_mean(t, col::EGAIN, &grows), 2));

            // Two sentinels mean "no filter": 'No Filter' (the [defaults]
            // value, injected when the file never had a FILTER column) and
            // 'None' (AggregationStep's per-cell null fill).
            let filter_val = if filter_matters {
                let v = astype_str(&gkey[0]);
                if v == "No Filter" || v == "None" {
                    String::new()
                } else {
                    v
                }
            } else {
                String::new()
            };

            lines.push(row_fmt(
                &CAL_HEADER_WIDTHS,
                &[
                    filter_val,
                    number.to_string(),
                    gain_str,
                    egain_str,
                    format!("{} secs", fmt_f(duration, 2)),
                    seconds_to_hms(row_total, true),
                ],
            ));
        }
    }

    (lines.join("\n"), total_exposure)
}

/// `generate_full_summary`.
pub fn generate_full_summary(df: &Table, total_scanned: usize, now: &str) -> String {
    if df.n_rows == 0 {
        return "No data available for reporting.".to_string();
    }
    let mut report: Vec<String> = vec![format!("Observation session summary\nGenerated {now}")];

    let all_rows: Vec<usize> = (0..df.n_rows).collect();
    for (site_key, site_rows) in group_rows(df, &[col::SITE_NAME], &all_rows) {
        let site = astype_str(&site_key[0]);
        let lights = light_rows(df, &site_rows);
        if lights.is_empty() {
            continue; // sites with only calibration frames
        }

        let light_filters: HashSet<String> = match df.column(col::FILTER_NAME) {
            Some(c) => lights
                .iter()
                .map(|&i| astype_str(&c.cells[i]).to_lowercase())
                .collect(),
            None => HashSet::new(),
        };
        let light_gains: HashSet<String> = match df.column(col::GAIN_MATCH) {
            Some(c) => lights.iter().map(|&i| key_of(&c.cells[i])).collect(),
            None => HashSet::new(),
        };

        report.push(get_target_details(df, &lights));
        report.push(format!("\nSite: {site}"));
        // `.iloc[0]` on the *site* group, calibration rows included.
        let scalar = |name: &str, digits: usize| -> String {
            fmt_f(
                iloc0(df, name, &site_rows)
                    .and_then(|c| to_numeric(&c))
                    .unwrap_or(f64::NAN),
                digits,
            )
        };
        report.push(format!("\tLatitude: {}\u{b0}", scalar(col::SITE_LAT, 4)));
        report.push(format!("\tLongitude: {}\u{b0}", scalar(col::SITE_LONG, 4)));
        report.push(format!("\tBortle scale: {}", scalar(col::BORTLE, 1)));
        report.push(format!(
            "\tSQM: {} mag/arcsec\u{b2}",
            scalar(col::MEAN_SQM, 2)
        ));

        report.push(get_equipment_used(df, &lights));
        report.push(get_observation_period(df, &lights));

        let order: [(&str, &[&str]); 5] = [
            (it::LIGHT, &[it::LIGHT]),
            (it::FLAT, &[it::FLAT, it::MASTER_FLAT]),
            (it::BIAS, &[it::BIAS, it::MASTER_BIAS]),
            (it::DARK, &[it::DARK, it::MASTER_DARK]),
            (it::DARK_FLAT, &[it::DARK_FLAT, it::MASTER_DARKFLAT]),
        ];
        for (primary_type, type_tuple) in order {
            let category: Vec<usize> = match df.column(col::IMAGE_TYPE) {
                Some(c) => site_rows
                    .iter()
                    .copied()
                    .filter(|&i| {
                        matches!(&c.cells[i], Cell::Str(s) if type_tuple.contains(&s.as_str()))
                    })
                    .collect(),
                None => Vec::new(),
            };
            if category.is_empty() {
                continue;
            }
            let (table, exp) = format_image_type_table(
                df,
                &category,
                primary_type,
                Some(&light_filters),
                Some(&light_gains),
            );
            if !table.is_empty() {
                report.push(table);
                report.push(format!(
                    "\nTotal {primary_type} Exposure Time: {}\n",
                    seconds_to_hms(exp, false)
                ));
            }
        }
    }

    report.push(format!(
        "\n Total number of images processed: {total_scanned}\n"
    ));
    report.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::{Column, DType};

    #[test]
    fn hms_aligned_pads_each_field_to_six() {
        assert_eq!(
            seconds_to_hms(1020.0, true),
            "     0 hrs     17 mins   0.00 secs"
        );
        assert_eq!(seconds_to_hms(97500.0, false), "27 hrs 5 mins 0.00 secs");
        assert_eq!(seconds_to_hms(51.0, true), "     0 hrs      0 mins  51.00 secs");
    }

    #[test]
    fn python_floor_modulo_not_rust_truncated_modulo() {
        // Python: divmod(-90.0, 60) == (-2.0, 30.0); Rust's % gives -30.0.
        let (d, m) = py_divmod(-90.0, 60.0);
        assert_eq!((d, m), (-2.0, 30.0));
    }

    #[test]
    fn nan_prints_lower_case_like_python() {
        assert_eq!(fmt_f(f64::NAN, 2), "nan");
        assert_eq!(fmt_f(-10.0, 1), "-10.0");
    }

    #[test]
    fn a_mosaic_collapses_its_panels_into_one_target_line() {
        let t = Table::parse_str(
            "object\nNGC 6997 Panel 1\nNGC 6997 Panel 2\nNGC 6997 Panel 1\n",
        )
        .unwrap();
        assert_eq!(
            get_target_details(&t, &[0, 1, 2]),
            " Target: NGC 6997 2 Panel Mosaic"
        );
    }

    #[test]
    fn without_panels_the_first_distinct_target_wins() {
        let t = Table::parse_str("object\nSh2 101\nSh2 101\n").unwrap();
        assert_eq!(get_target_details(&t, &[0, 1]), " Target: Sh2 101");
        assert_eq!(get_target_details(&t, &[]), " Target: No target data");
    }

    /// A calibration frame with just enough columns for
    /// `format_image_type_table`'s non-LIGHT branch.
    fn cal_row(imagetyp: &str) -> String {
        format!("{imagetyp},Ha,4663,100,0.25,600.0,10")
    }

    fn cal_table(rows: &[&str]) -> Table {
        let header = "imagetyp,filter,filter_code,gain,egain,exposure,number";
        let mut text = String::from(header);
        for r in rows {
            text.push('\n');
            text.push_str(r);
        }
        text.push('\n');
        let mut t = Table::parse_str(&text).unwrap();
        // gain_match is normally CalibrationMatcherStep's output; the label
        // logic under test never reads it directly, but format_image_type_table
        // does when light_gains/light_filters are Some, so give every row the
        // same key and pass None for both filters to skip that path.
        t.set_column(
            col::GAIN_MATCH,
            Column {
                name: col::GAIN_MATCH.into(),
                dtype: DType::Str,
                cells: vec![Cell::Str("G_100".into()); t.n_rows],
            },
        );
        t
    }

    /// PORT_PLAN.md / CHANGELOG.md 2.1.2: the label reflects what the table
    /// actually contains, not a blind lookup on the category's base type.
    #[test]
    fn calibration_section_label_reflects_what_is_actually_in_the_table() {
        // Pure raw: every DARK row, no MASTER anywhere -- plain label.
        let raw = cal_table(&[&cal_row("DARK"), &cal_row("DARK")]);
        let (text, _) = format_image_type_table(&raw, &[0, 1], "DARK", None, None);
        assert!(text.contains("\n DARKS:\n"), "{text}");
        assert!(!text.contains("MASTERDARKS"), "{text}");

        // Pure master -- MASTER label.
        let master = cal_table(&[&cal_row("MASTERDARK"), &cal_row("MASTERDARK")]);
        let (text, _) = format_image_type_table(&master, &[0, 1], "DARK", None, None);
        assert!(text.contains("\n MASTERDARKS:\n"), "{text}");

        // Mixed: a real master alongside a surviving raw frame -- MASTER
        // label, the safer thing to over-claim toward on a non-uniform table.
        let mixed = cal_table(&[&cal_row("MASTERDARK"), &cal_row("DARK")]);
        let (text, _) = format_image_type_table(&mixed, &[0, 1], "DARK", None, None);
        assert!(text.contains("\n MASTERDARKS:\n"), "{text}");

        // BIAS takes no extra S, matching the label the old lookup table
        // also used ('MASTERBIAS', never 'MASTERBIASS').
        let bias = cal_table(&[&cal_row("BIAS")]);
        let (text, _) = format_image_type_table(&bias, &[0], "BIAS", None, None);
        assert!(text.contains("\n BIAS:\n"), "{text}");
        assert!(!text.contains("BIASS"), "{text}");
    }

    /// Upstream issue #9 (github.com/SteveGreaves/AstroBinUploader): two
    /// MASTERDARK files at the same gain but different exposures (180s and
    /// 600s -- the reporter's own example) merged into one calibration
    /// table row, showing one exposure and a combined frame count. Darks
    /// are filter-independent, so their group key is (gain, duration) --
    /// duration was already part of it, and the merge did not reproduce
    /// when checked directly: verified end to end against live Python
    /// v2.1.2 with a --test CSV built from the report's own numbers
    /// (32 subs at 180s, 24 at 600s) before this test was written, so this
    /// pins the same behaviour the oracle already confirmed rather than an
    /// assumption about what the fix should look like.
    #[test]
    fn github_issue_9_different_exposure_masters_stay_separate() {
        let t = cal_table(&[
            "MASTERDARK,,,100,1.0,180.0,32",
            "MASTERDARK,,,100,1.0,600.0,24",
        ]);
        let (text, total_exposure) = format_image_type_table(&t, &[0, 1], "DARK", None, None);
        assert!(text.contains("32"), "{text}");
        assert!(text.contains("180.00 secs"), "{text}");
        assert!(text.contains("24"), "{text}");
        assert!(text.contains("600.00 secs"), "{text}");
        // 32*180 + 24*600 = 5760 + 14400 = 20160, not a merged single row's
        // worth of exposure.
        assert_eq!(total_exposure, 20160.0);
    }
}

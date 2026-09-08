//! `AggregationStep` — thousands of frames down to one row per session.
//!
//! Rows are grouped on eight keys (site, session date, image type, filter,
//! gain, binning, exposure, target) and each remaining column is reduced by
//! its own rule. Three things here decide the output beyond the arithmetic:
//!
//! - **The sort is stable and `NaT` sorts last.** Every `'first'` rule reads
//!   from this order, so an unstable sort would make the exported camera,
//!   telescope and filename depend on nothing in particular (A9 upstream).
//! - **`agg('first')` skips nulls**, unlike `.iloc[0]`. A group whose first
//!   row has no focuser name still exports the next row's.
//! - **`'mean'` is Kahan-compensated**, because it goes through a groupby —
//!   see `steps::kahan_mean`.
//!
//! Session dates are the subtle part. With `USEOBSDATE = False` (as in the
//! golden config, so both fixtures exercise it) a frame is dated by the night
//! it belongs to rather than the calendar day it was written: rows are cut
//! into sessions wherever there is a gap longer than five hours, and every row
//! in a session takes the date of that session's *first* frame — shifted back
//! a day when that frame was taken before noon, so an 02:00 exposure belongs
//! to the night that started the previous evening.

use anyhow::{bail, Result};

use crate::appconfig::AppConfig;
use crate::constants as col;
use crate::constants::image_type::LIGHT;
use crate::datetime::{self, Timestamp};
use crate::steps::{astype_str, kahan_mean, kahan_sum, to_numeric};
use crate::table::{Cell, Column, DType, Table};

/// How each aggregated column is reduced, and from which source column.
enum Rule {
    Sum,
    Mean,
    Min,
    Max,
    First,
}

pub fn execute(table: &Table, cfg: &AppConfig) -> Result<Table> {
    if table.n_rows == 0 {
        return Ok(table.clone());
    }

    // --- Stage 1: temporal normalisation ------------------------------------
    let Some(date_col) = table.column(col::DATE_OBS) else {
        bail!("aggregation needs a '{}' column", col::DATE_OBS);
    };
    let parsed: Vec<Option<Timestamp>> = date_col
        .cells
        .iter()
        .map(|c| datetime::parse(&astype_str(c)))
        .collect();

    // Stable sort, NaT last.
    let mut order: Vec<usize> = (0..table.n_rows).collect();
    order.sort_by_key(|&i| (parsed[i].is_none(), parsed[i].unwrap_or(0)));
    let mut df = table.take_rows(&order);
    let dates: Vec<Option<Timestamp>> = order.iter().map(|&i| parsed[i]).collect();

    let Some(itype) = df.column(col::IMAGE_TYPE) else {
        bail!("aggregation needs an '{}' column", col::IMAGE_TYPE);
    };
    let is_light: Vec<bool> = itype
        .cells
        .iter()
        .map(|c| matches!(c, Cell::Str(s) if s == LIGHT))
        .collect();

    // Global session statistics, from light frames only.
    let light_dates: Vec<Timestamp> = (0..df.n_rows)
        .filter(|&i| is_light[i])
        .filter_map(|i| dates[i])
        .collect();
    let any_lights = (0..df.n_rows).any(|i| is_light[i]);

    let (session_count, start_date, end_date, num_days) = if any_lights {
        // `diff() > 5h` then `cumsum().max() + 1`: the number of gaps plus one.
        let mut gaps = 0i64;
        let ordered: Vec<Timestamp> = (0..df.n_rows)
            .filter(|&i| is_light[i])
            .filter_map(|i| dates[i])
            .collect();
        for w in ordered.windows(2) {
            if w[1] - w[0] > datetime::SESSION_GAP_NS {
                gaps += 1;
            }
        }
        match (light_dates.iter().min(), light_dates.iter().max()) {
            (Some(&lo), Some(&hi)) => (
                gaps + 1,
                datetime::date_string(lo),
                datetime::date_string(hi),
                datetime::days(hi) - datetime::days(lo) + 1,
            ),
            // Lights exist but none has a usable date: `.min()` is NaT and
            // `.strftime` on it raises, so this cannot be reached from the
            // Python either. Fail loudly rather than invent a date.
            _ => bail!("light frames are present but none has a parseable DATE-OBS"),
        }
    } else {
        (0, "N/A".to_string(), "N/A".to_string(), 0)
    };

    // --- Stage 2: session dates ---------------------------------------------
    let session_date: Vec<String> = if cfg.use_obs_date {
        dates
            .iter()
            .map(|d| match d {
                Some(ts) => datetime::date_string(*ts),
                // `.dt.date` on NaT is NaT, which the group-key fill then
                // turns into the string "None".
                None => "None".to_string(),
            })
            .collect()
    } else {
        // Cut into sessions across *all* frames, not just lights, then date
        // every row by its session's first timestamp.
        let mut ids: Vec<usize> = Vec::with_capacity(df.n_rows);
        let mut id = 0usize;
        // `prev` holds the last non-null date, while pandas' `.diff()`
        // compares against the immediately preceding row. The two agree only
        // because the sort above puts every NaT at the end, so a real date
        // never follows one; change that sort and this has to change with it.
        let mut prev: Option<Timestamp> = None;
        for d in &dates {
            if let (Some(p), Some(c)) = (prev, *d) {
                if c - p > datetime::SESSION_GAP_NS {
                    id += 1;
                }
            }
            ids.push(id);
            if d.is_some() {
                prev = *d;
            }
        }
        // `transform('first')` takes the first *non-null* value in each group,
        // which need not be the group's first row.
        let mut firsts: Vec<Option<Timestamp>> = vec![None; id + 1];
        for (i, d) in dates.iter().enumerate() {
            if firsts[ids[i]].is_none() {
                firsts[ids[i]] = *d;
            }
        }
        ids.iter()
            .map(|&g| match firsts[g] {
                Some(ts) => {
                    let shifted = if datetime::time_of_day_ns(ts) < datetime::NOON_NS {
                        datetime::minus_one_day(ts)
                    } else {
                        ts
                    };
                    datetime::date_string(shifted)
                }
                None => "None".to_string(),
            })
            .collect()
    };

    df.set_column(
        "session_date",
        Column {
            name: "session_date".into(),
            dtype: DType::Str,
            cells: session_date.iter().map(|s| Cell::Str(s.clone())).collect(),
        },
    );
    df.set_scalar("sessions", Cell::Int(session_count));
    df.set_scalar("start_date", Cell::Str(start_date));
    df.set_scalar("end_date", Cell::Str(end_date));
    df.set_scalar("num_days", Cell::Int(num_days));

    // --- Stage 3: aggregation -----------------------------------------------
    let agg_cols = [
        col::SITE_NAME,
        "session_date",
        col::IMAGE_TYPE,
        col::FILTER_NAME,
        col::GAIN,
        col::BINNING,
        col::DURATION,
        col::TARGET,
    ];

    // A null in any group key would make pandas drop the row outright, so
    // every key gets a fallback first: 0 for a numeric column, the *string*
    // "None" for anything else. Filling a numeric key with "None" would
    // promote it to object and turn `100` into `100.0` in the export (A5).
    for name in agg_cols {
        match df.column(name) {
            None => df.set_scalar(name, Cell::Str("None".into())),
            Some(c) => {
                let numeric = matches!(c.dtype, DType::Int | DType::Float | DType::Bool);
                let fill = if numeric {
                    Cell::Int(0)
                } else {
                    Cell::Str("None".into())
                };
                if c.cells.iter().any(|v| v.is_null()) {
                    let dtype = c.dtype;
                    let cells = c
                        .cells
                        .iter()
                        .map(|v| if v.is_null() { fill.clone() } else { v.clone() })
                        .collect();
                    df.set_column(
                        name,
                        Column {
                            name: name.to_string(),
                            dtype,
                            cells,
                        },
                    );
                }
            }
        }
    }

    // `to_numeric(...).fillna(0.0)` on the columns about to be averaged. An
    // int64 column stays int64 -- it has no nulls to fill.
    for name in [
        col::SENSOR_COOLING,
        col::MEAN_FWHM,
        col::SITE_LAT,
        col::SITE_LONG,
        col::F_NUMBER,
        col::TEMPERATURE,
        col::BORTLE,
        col::MEAN_SQM,
        col::EGAIN,
        "darks",
        "flats",
        "flatDarks",
        "bias",
    ] {
        let Some(c) = df.column(name) else { continue };
        if c.dtype == DType::Int && !c.cells.iter().any(|v| v.is_null()) {
            continue;
        }
        let cells = c
            .cells
            .iter()
            .map(|v| Cell::Float(to_numeric(v).unwrap_or(0.0)))
            .collect();
        df.set_column(
            name,
            Column {
                name: name.to_string(),
                dtype: DType::Float,
                cells,
            },
        );
    }

    // Groups come out sorted by the key tuple, each column compared by its own
    // type -- which is why the key carries the cell rather than a rendering of
    // it.
    let key_cols: Vec<&Column> = agg_cols
        .iter()
        .map(|n| df.column(n).expect("group key was just ensured"))
        .collect();
    let mut keyed: Vec<(Vec<Cell>, usize)> = (0..df.n_rows)
        .map(|i| (key_cols.iter().map(|c| c.cells[i].clone()).collect(), i))
        .collect();
    keyed.sort_by(|a, b| compare_keys(&a.0, &b.0));

    let mut groups: Vec<(Vec<Cell>, Vec<usize>)> = Vec::new();
    for (key, row) in keyed {
        match groups.last_mut() {
            Some((k, rows)) if *k == key => rows.push(row),
            _ => groups.push((key, vec![row])),
        }
    }

    let rules: &[(&str, &str, Rule)] = &[
        (col::NUMBER, col::NUMBER, Rule::Sum),
        (col::SENSOR_COOLING, col::SENSOR_COOLING, Rule::Mean),
        ("temp_min", col::TEMPERATURE, Rule::Min),
        ("temp_max", col::TEMPERATURE, Rule::Max),
        (col::MEAN_FWHM, col::MEAN_FWHM, Rule::Mean),
        (col::SITE_LAT, col::SITE_LAT, Rule::Mean),
        (col::SITE_LONG, col::SITE_LONG, Rule::Mean),
        (col::F_NUMBER, col::F_NUMBER, Rule::Mean),
        (col::TEMPERATURE, col::TEMPERATURE, Rule::Mean),
        (col::FOCAL_LENGTH, col::FOCAL_LENGTH, Rule::First),
        (col::BORTLE, col::BORTLE, Rule::Mean),
        (col::MEAN_SQM, col::MEAN_SQM, Rule::Mean),
        (col::PIXEL_SIZE, col::PIXEL_SIZE, Rule::First),
        (col::GAIN_MATCH, col::GAIN_MATCH, Rule::First),
        (col::EGAIN, col::EGAIN, Rule::Mean),
        (col::CAMERA, col::CAMERA, Rule::First),
        (col::TELESCOPE, col::TELESCOPE, Rule::First),
        (col::FOCUSER, col::FOCUSER, Rule::First),
        (col::FILTER_WHEEL, col::FILTER_WHEEL, Rule::First),
        (col::ROTATOR_NAME, col::ROTATOR_NAME, Rule::First),
        (col::SWCREATE, col::SWCREATE, Rule::First),
        (col::FILENAME, col::FILENAME, Rule::First),
        ("sessions", "sessions", Rule::First),
        ("start_date", "start_date", Rule::First),
        ("end_date", "end_date", Rule::First),
        ("num_days", "num_days", Rule::First),
        ("darks", "darks", Rule::Max),
        ("flats", "flats", Rule::Max),
        ("flatDarks", "flatDarks", Rule::Max),
        ("bias", "bias", Rule::Max),
    ];

    let mut out = Table {
        columns: Vec::new(),
        n_rows: groups.len(),
    };

    // The group keys come back as columns first (`reset_index()`), in
    // `agg_cols` order, then one column per rule in the rules' order.
    for (k, name) in agg_cols.iter().enumerate() {
        out.columns.push(Column {
            name: (*name).to_string(),
            dtype: key_cols[k].dtype,
            cells: groups.iter().map(|(key, _)| key[k].clone()).collect(),
        });
    }

    for (name, source, rule) in rules {
        let Some(src) = df.column(source) else {
            bail!("aggregation rule '{name}' needs a '{source}' column");
        };
        let cells: Vec<Cell> = groups
            .iter()
            .map(|(_, rows)| reduce(rule, src, rows))
            .collect();
        let dtype = match rule {
            // A mean is always float64, whatever it was averaging.
            Rule::Mean => DType::Float,
            _ => src.dtype,
        };
        out.columns.push(Column {
            name: (*name).to_string(),
            dtype,
            cells,
        });
    }

    // --- Stage 4: filter codes ----------------------------------------------
    // `str(name).strip()` against the `[filters]` section; an unmapped filter
    // exports its own name, which is what AstroBin shows when it cannot match
    // a code.
    if let Some(filters) = out.column(col::FILTER_NAME) {
        let cells: Vec<Cell> = filters
            .cells
            .iter()
            .map(|c| {
                let name = astype_str(c);
                let name = name.trim();
                let code = cfg
                    .filters
                    .iter()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v.as_str())
                    .unwrap_or_else(|| name.to_string());
                Cell::Str(code)
            })
            .collect();
        out.columns.push(Column {
            name: "filter_code".into(),
            dtype: DType::Str,
            cells,
        });
    }

    Ok(out)
}

/// Element-wise comparison of two group keys, each column by its own type —
/// what `groupby(sort=True)` does across a mixed-dtype key.
pub fn compare_keys(a: &[Cell], b: &[Cell]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for (x, y) in a.iter().zip(b) {
        let ord = match (x, y) {
            (Cell::Int(p), Cell::Int(q)) => p.cmp(q),
            (Cell::Float(p), Cell::Float(q)) => p.partial_cmp(q).unwrap_or(Ordering::Equal),
            (Cell::Str(p), Cell::Str(q)) => p.cmp(q),
            (Cell::Bool(p), Cell::Bool(q)) => p.cmp(q),
            // Mixed types in one key column would raise in pandas ("'<' not
            // supported between instances of ..."), and the group-key fill
            // above makes it unreachable; treat as equal rather than panic.
            _ => Ordering::Equal,
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

/// One group, one column, one reduction. Every rule skips nulls, which is
/// what makes `agg('first')` different from `.iloc[0]`.
fn reduce(rule: &Rule, src: &Column, rows: &[usize]) -> Cell {
    match rule {
        Rule::First => rows
            .iter()
            .map(|&i| &src.cells[i])
            .find(|c| !c.is_null())
            .cloned()
            .unwrap_or(Cell::Null),
        Rule::Mean => {
            let values: Vec<f64> = rows.iter().filter_map(|&i| to_numeric(&src.cells[i])).collect();
            if values.is_empty() {
                Cell::Float(f64::NAN)
            } else {
                Cell::Float(kahan_mean(values))
            }
        }
        Rule::Sum => {
            let values: Vec<f64> = rows.iter().filter_map(|&i| to_numeric(&src.cells[i])).collect();
            match src.dtype {
                DType::Int => Cell::Int(values.iter().map(|v| *v as i64).sum()),
                // Unreachable today -- the only `Sum` rule sources `number`,
                // which stage 7 hardens to int64 unconditionally -- but a
                // naive fold here would be a silent parity bug the moment it
                // stopped being unreachable.
                _ => Cell::Float(kahan_sum(values.iter().copied())),
            }
        }
        Rule::Min | Rule::Max => {
            let mut best: Option<&Cell> = None;
            for &i in rows {
                let c = &src.cells[i];
                if c.is_null() {
                    continue;
                }
                best = Some(match best {
                    None => c,
                    Some(b) => {
                        let take = match (rule, cell_ord(c, b)) {
                            (Rule::Min, std::cmp::Ordering::Less) => true,
                            (Rule::Max, std::cmp::Ordering::Greater) => true,
                            _ => false,
                        };
                        if take {
                            c
                        } else {
                            b
                        }
                    }
                });
            }
            match best {
                Some(c) => c.clone(),
                None if src.dtype == DType::Str => Cell::Null,
                None => Cell::Float(f64::NAN),
            }
        }
    }
}

fn cell_ord(a: &Cell, b: &Cell) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (to_numeric(a), to_numeric(b)) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
        _ => astype_str(a).cmp(&astype_str(b)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigFile;

    fn cfg(text: &str) -> AppConfig {
        AppConfig::from_config(&ConfigFile::parse_str(text).unwrap()).unwrap()
    }

    /// A frame with every column the rules need, one row per line of `rows`.
    fn frame(rows: &[&str]) -> Table {
        let header = "site,imagetyp,filter,gain,xbinning,exposure,object,date-obs,number,\
                      ccd-temp,foctemp,fwhm,sitelat,sitelong,focratio,focallen,bortle,sqm,\
                      xpixsz,gain_match,egain,instrume,telescop,focname,fwheel,rotname,\
                      swcreate,filename,darks,flats,flatDarks,bias";
        let mut text = String::from(header);
        for r in rows {
            text.push('\n');
            text.push_str(r);
        }
        text.push('\n');
        Table::parse_str(&text).unwrap()
    }

    const A: &str = "Home,LIGHT,Ha,100,1.0,600.0,M31,2023-07-06T22:00:00,1,\
                     -10.0,20.0,2.0,52.0,-0.1,5.0,540.0,4.0,21.0,3.76,E_0.25,0.25,\
                     Cam,Scope,Foc,Wheel,Rot,NINA,a.fits,10,20,30,40";
    const B: &str = "Home,LIGHT,Ha,100,1.0,600.0,M31,2023-07-07T02:00:00,1,\
                     -12.0,22.0,3.0,52.0,-0.1,5.0,540.0,4.0,21.0,3.76,E_0.25,0.25,\
                     Cam,Scope,Foc,Wheel,Rot,NINA,b.fits,10,20,30,40";

    #[test]
    fn one_night_across_midnight_is_one_session_date() {
        // USEOBSDATE = False: the 02:00 frame belongs to the night that began
        // the previous evening, so both rows collapse into one group.
        let out = execute(
            &frame(&[A, B]),
            &cfg("[defaults]\nUSEOBSDATE = False\n[filters]\nHa = 4663\n"),
        )
        .unwrap();
        assert_eq!(out.n_rows, 1);
        assert_eq!(
            out.column("session_date").unwrap().cells[0],
            Cell::Str("2023-07-06".into())
        );
        assert_eq!(out.column("number").unwrap().cells[0], Cell::Int(2));
        assert_eq!(out.column("filter_code").unwrap().cells[0], Cell::Str("4663".into()));
    }

    #[test]
    fn use_obs_date_true_splits_the_same_night_by_calendar_day() {
        let out = execute(
            &frame(&[A, B]),
            &cfg("[defaults]\nUSEOBSDATE = True\n[filters]\nHa = 4663\n"),
        )
        .unwrap();
        assert_eq!(out.n_rows, 2);
    }

    #[test]
    fn reductions_follow_their_own_rules() {
        let out = execute(
            &frame(&[A, B]),
            &cfg("[defaults]\nUSEOBSDATE = False\n[filters]\nHa = 4663\n"),
        )
        .unwrap();
        // mean of -10 and -12
        assert_eq!(out.column("ccd-temp").unwrap().cells[0], Cell::Float(-11.0));
        assert_eq!(out.column("temp_min").unwrap().cells[0], Cell::Float(20.0));
        assert_eq!(out.column("temp_max").unwrap().cells[0], Cell::Float(22.0));
        // 'first' reads the earliest row by DATE-OBS
        assert_eq!(
            out.column("filename").unwrap().cells[0],
            Cell::Str("a.fits".into())
        );
        // counters take the maximum, not the sum
        assert_eq!(out.column("darks").unwrap().cells[0], Cell::Int(10));
        assert_eq!(out.column("sessions").unwrap().cells[0], Cell::Int(1));
        assert_eq!(out.column("num_days").unwrap().cells[0], Cell::Int(2));
    }

    #[test]
    fn first_skips_a_null_rather_than_exporting_it() {
        let missing = A.replace(",Cam,", ",,");
        let out = execute(
            &frame(&[&missing, B]),
            &cfg("[defaults]\nUSEOBSDATE = False\n[filters]\nHa = 4663\n"),
        )
        .unwrap();
        assert_eq!(
            out.column("instrume").unwrap().cells[0],
            Cell::Str("Cam".into())
        );
    }

    #[test]
    fn an_unmapped_filter_exports_its_own_name() {
        let out = execute(&frame(&[A]), &cfg("[defaults]\nUSEOBSDATE = False\n")).unwrap();
        assert_eq!(out.column("filter_code").unwrap().cells[0], Cell::Str("Ha".into()));
    }

    #[test]
    fn a_five_hour_gap_starts_a_new_session() {
        let late = B.replace("2023-07-07T02:00:00", "2023-07-07T21:00:00");
        let out = execute(
            &frame(&[A, &late]),
            &cfg("[defaults]\nUSEOBSDATE = False\n[filters]\nHa = 4663\n"),
        )
        .unwrap();
        assert_eq!(out.n_rows, 2);
        assert_eq!(out.column("sessions").unwrap().cells[0], Cell::Int(2));
    }
}

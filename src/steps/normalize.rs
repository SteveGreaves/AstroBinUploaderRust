//! `NormalizeHeadersStep` — seven stages, from `engine/steps/base.py`.
//!
//! The largest step in the pipeline and the one with the most ways to be
//! subtly wrong. The stages that bite, in the order they run:
//!
//! 2. Lower-casing the column names can create duplicates, and coalescing
//!    them **sorts the columns** — but only when duplicates actually exist.
//!    No duplicates, original order.
//! 3. A default is injected only if its lower-cased key is *still* absent, and
//!    it is injected as the configobj **string**: `[defaults] ROTANTANG = 0`
//!    makes an `object` column of `"0"`, not a number.
//! 3b. `[equipmentoverrides]` runs after defaults and overwrites the whole
//!    column, found value or not.
//! 5. Master preference recombines as `concat([lights, cals])`, which
//!    **reorders every row**: all lights first, then calibration frames in
//!    group-key order.
//! 6. The `IMAGETYP` keyword map is applied longest-keyword-first against a
//!    *frozen* copy, with an `assigned` mask so each row is rewritten once.
//!    Without the freeze, `DARK` re-matches the `MASTERDARK` it just wrote
//!    (A13 upstream).
//! 7. Hardening is asymmetric per column: a float list via `astype(float)`,
//!    `exposure` via pandas `.round(2)`, `gain` via pandas `.round()` then
//!    `astype(int)`, `number` via `fillna(1).astype(int)`, `site` via
//!    `astype(str).replace('nan', default)`. Everything else is left alone —
//!    `foctemp`, `object` and `filter` are core columns but are *not* cast.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;

use crate::appconfig::AppConfig;
use crate::constants as col;
use crate::numeric::numpy_round;
use crate::steps::{astype_str, cast, promote, python_float, to_numeric};
use crate::table::{Cell, Column, DType, Table};

pub fn execute(table: &Table, cfg: &AppConfig) -> Result<Table> {
    let mut df = table.clone();

    stage1_hardware_overrides(&mut df, cfg)?;
    stage2_lowercase_and_coalesce(&mut df)?;
    stage3_defaults(&mut df, cfg)?;
    stage3b_equipment_overrides(&mut df, cfg);
    let mut df = stage4_initial_filter(df);
    let mut df = stage5_master_preference(&mut df)?;
    stage6_normalize_image_type(&mut df);
    stage7_harden(&mut df);

    Ok(df)
}

/// Stage 1 — map hardware keywords onto internal keys.
///
/// Overrides are applied in config order, and within one override the
/// candidate list is a priority chain: the first matching column supplies the
/// values, later ones only fill its gaps. The source columns are then dropped,
/// unless a source *is* the target.
fn stage1_hardware_overrides(df: &mut Table, cfg: &AppConfig) -> Result<()> {
    for (internal_key, hw_keys) in &cfg.overrides {
        let mut combined: Option<Column> = None;
        let mut found: Vec<String> = Vec::new();

        for hw_key in hw_keys {
            let Some(source) = df.column_ignore_case(hw_key) else {
                continue;
            };
            found.push(source.name.clone());
            combined = Some(match combined {
                None => source.clone(),
                Some(acc) => fillna_from(&acc, source).with_context(|| {
                    format!(
                        "[override] {internal_key}: coalescing '{}' into the \
                         accumulated value",
                        source.name
                    )
                })?,
            });
        }

        if let Some(c) = combined {
            df.set_column(internal_key, c);
            for name in found {
                if name != *internal_key {
                    df.drop_column(&name);
                }
            }
        }
    }
    Ok(())
}

/// `a.fillna(b)` — `a`'s value where present, `b`'s where `a` is null.
fn fillna_from(a: &Column, b: &Column) -> Result<Column> {
    let dtype = promote(a.dtype, b.dtype).ok_or_else(|| {
        anyhow::anyhow!(
            "filling a {:?} column from a {:?} one would make an object column \
             in pandas, and reproducing Python's repr for the numbers in it is \
             not implemented (no fixture reaches this)",
            a.dtype,
            b.dtype
        )
    })?;
    let cells = a
        .cells
        .iter()
        .zip(&b.cells)
        .map(|(x, y)| cast(if x.is_null() { y } else { x }, dtype))
        .collect();
    Ok(Column {
        name: a.name.clone(),
        dtype,
        cells,
    })
}

/// Stage 2 — lower-case every column name, then merge any duplicates the
/// lower-casing created.
fn stage2_lowercase_and_coalesce(df: &mut Table) -> Result<()> {
    for c in &mut df.columns {
        c.name = c.name.to_lowercase();
    }

    let mut seen: HashMap<&str, usize> = HashMap::new();
    let mut duplicated = false;
    for c in &df.columns {
        if seen.insert(c.name.as_str(), 0).is_some() {
            duplicated = true;
            break;
        }
    }
    if !duplicated {
        return Ok(());
    }

    // `_coalesce_duplicate_columns`: first non-null across each same-named
    // group, left to right, and the surviving columns come out **sorted** --
    // that is what the `groupby(level=0, axis=1).first()` it replaced did.
    let mut names: Vec<String> = Vec::new();
    for c in &df.columns {
        if !names.contains(&c.name) {
            names.push(c.name.clone());
        }
    }
    names.sort();

    let mut merged: Vec<Column> = Vec::with_capacity(names.len());
    for name in names {
        let group: Vec<&Column> = df.columns.iter().filter(|c| c.name == name).collect();
        if group.len() == 1 {
            merged.push(group[0].clone());
            continue;
        }
        let mut acc = group[0].clone();
        for next in &group[1..] {
            acc = fillna_from(&acc, next)
                .with_context(|| format!("coalescing duplicate columns named '{name}'"))?;
        }
        merged.push(acc);
    }
    df.columns = merged;
    Ok(())
}

/// Stage 3 — inject a default for every core key still missing.
fn stage3_defaults(df: &mut Table, cfg: &AppConfig) -> Result<()> {
    for (key, value) in &cfg.defaults {
        let lower = key.to_lowercase();
        if df.has_column(&lower) {
            continue;
        }
        let scalar = AppConfig::default_scalar(key, value)?;
        df.set_scalar(&lower, Cell::Str(scalar));
    }
    Ok(())
}

/// Stage 3b — force literal display values in, over defaults and found values
/// alike.
fn stage3b_equipment_overrides(df: &mut Table, cfg: &AppConfig) {
    for (key, value) in &cfg.equipment_overrides {
        df.set_scalar(&key.to_lowercase(), Cell::Str(value.clone()));
    }
}

/// Stage 4 — upper-case `imagetyp` and drop master lights and missing types.
///
/// Masters of *light* frames are dropped because totals are computed from the
/// individual subs; keeping both would double every integration time.
fn stage4_initial_filter(mut df: Table) -> Table {
    if !df.has_column(col::IMAGE_TYPE) {
        return df;
    }
    let upper: Vec<String> = df
        .column(col::IMAGE_TYPE)
        .unwrap()
        .cells
        .iter()
        .map(|c| astype_str(c).to_uppercase())
        .collect();

    df.set_column(
        col::IMAGE_TYPE,
        Column {
            name: col::IMAGE_TYPE.to_string(),
            dtype: DType::Str,
            cells: upper.iter().map(|s| Cell::Str(s.clone())).collect(),
        },
    );

    let keep: Vec<usize> = upper
        .iter()
        .enumerate()
        .filter(|(_, v)| !(v.contains("MASTERLIGHT") || v.contains("MASTER LIGHT") || *v == "NAN"))
        .map(|(i, _)| i)
        .collect();

    if keep.len() == df.n_rows {
        return df;
    }
    df.take_rows(&keep)
}

/// The master-preference grouping key.
///
/// Field order is the Python tuple's order, so the derived `Ord` sorts group
/// keys exactly as `groupby(sort=True)` does.
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Debug)]
struct GroupKey {
    base_type: String,
    gain: i64,
    egain: String,
    binning: String,
    /// Exposure for `DARK`/`BIAS` (which are filter-independent), filter name
    /// for everything else.
    tail: String,
}

/// Stage 5 — where a calibration group contains a master, keep only the
/// master.
///
/// Returns a frame whose rows are **all lights first, then the surviving
/// calibration frames in group-key order** — `pd.concat([lights, cals],
/// ignore_index=True)`. The reorder happens even when nothing is dropped, so
/// it is visible on any dataset with calibration frames. The one exception is
/// the early return when there are no calibration frames at all, which leaves
/// row order untouched.
fn stage5_master_preference(df: &mut Table) -> Result<Table> {
    let Some(itype) = df.column(col::IMAGE_TYPE) else {
        bail!("master preference needs an '{}' column", col::IMAGE_TYPE);
    };
    let types: Vec<String> = itype.cells.iter().map(astype_str).collect();

    let is_cal = |v: &str| -> bool {
        let u = v.to_uppercase();
        (u.contains("FLAT") || u.contains("DARK") || u.contains("BIAS")) && !u.contains("LIGHT")
    };

    let lights: Vec<usize> = (0..df.n_rows).filter(|&i| !is_cal(&types[i])).collect();
    let cals: Vec<usize> = (0..df.n_rows).filter(|&i| is_cal(&types[i])).collect();

    if cals.is_empty() {
        return Ok(df.clone());
    }

    let mut keyed: Vec<(GroupKey, usize)> = cals
        .iter()
        .map(|&i| Ok((group_key(df, i, &types[i])?, i)))
        .collect::<Result<_>>()?;
    // Stable so that within a group the original row order survives, which is
    // what pandas guarantees inside a group.
    keyed.sort_by(|a, b| a.0.cmp(&b.0));

    let mut chosen: Vec<usize> = Vec::new();
    let mut g = 0usize;
    while g < keyed.len() {
        let mut end = g + 1;
        while end < keyed.len() && keyed[end].0 == keyed[g].0 {
            end += 1;
        }
        let group: Vec<usize> = keyed[g..end].iter().map(|(_, i)| *i).collect();

        let masters: Vec<usize> = group
            .iter()
            .copied()
            .filter(|&i| types[i].to_uppercase().contains("MASTER"))
            .collect();

        if masters.is_empty() {
            chosen.extend(group); // no master: keep every raw frame
        } else if masters.len() == 1 {
            chosen.push(masters[0]);
        } else {
            // Several masters for one hardware group: keep the most recent by
            // DATE-OBS, falling back to the first under the scan order when
            // none of them parses (A10 upstream).
            let dates = df.column(col::DATE_OBS);
            let latest = masters
                .iter()
                .filter_map(|&i| {
                    dates
                        .and_then(|c| parse_iso_datetime(&astype_str(&c.cells[i])))
                        .map(|d| (d, i))
                })
                .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
            chosen.push(match latest {
                Some((_, i)) => i,
                None => masters[0],
            });
        }
        g = end;
    }

    let mut order = lights;
    order.extend(chosen);
    Ok(df.take_rows(&order))
}

fn group_key(df: &Table, row: usize, itype: &str) -> Result<GroupKey> {
    let orig = itype.to_uppercase();
    let base_type = orig.replace("MASTER", "").replace(' ', "").trim().to_string();

    let cell = |name: &str| -> Result<&Cell> {
        df.column(name)
            .map(|c| &c.cells[row])
            .ok_or_else(|| anyhow::anyhow!("master preference needs a '{name}' column"))
    };

    // `int(round(float(x)))`, with 0 for anything that raises. A NaN gets
    // there through `int(nan)`, which is a ValueError.
    let gain = match python_float(cell(col::GAIN)?) {
        Ok(v) if v.is_finite() => v.round_ties_even() as i64,
        _ => 0,
    };

    // `f"{float(x):.2f}"`. A NaN formats as "nan" rather than raising, so it
    // is a group key like any other.
    let egain = match python_float(cell(col::EGAIN)?) {
        Ok(v) => format_2f(v),
        Err(()) => "1.00".to_string(),
    };

    let binning = astype_str(cell(col::BINNING)?).trim().to_string();

    let tail = if base_type == "DARK" || base_type == "BIAS" {
        match python_float(cell(col::DURATION)?) {
            Ok(v) => format_2f(v),
            Err(()) => "0.00".to_string(),
        }
    } else {
        // `row.get('filter', ...)`: a missing column is the default here, not
        // an error.
        let raw = df
            .column(col::FILTER_NAME)
            .map(|c| astype_str(&c.cells[row]))
            .unwrap_or_else(|| "No Filter".to_string());
        let name = raw.to_lowercase();
        let name = name.trim();
        strip_filter_prefix(name).to_string()
    };

    Ok(GroupKey {
        base_type,
        gain,
        egain,
        binning,
        tail,
    })
}

/// `re.sub(r'^filter[_-]', '', name).strip()`.
fn strip_filter_prefix(name: &str) -> &str {
    for prefix in ["filter_", "filter-"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            return rest.trim();
        }
    }
    name
}

/// `f"{x:.2f}"` — Rust and Python both round half-to-even on the exact binary
/// value, and both spell a NaN `nan`.
fn format_2f(x: f64) -> String {
    if x.is_nan() {
        "nan".to_string()
    } else if x.is_infinite() {
        (if x > 0.0 { "inf" } else { "-inf" }).to_string()
    } else {
        format!("{x:.2}")
    }
}

/// Enough of `pd.to_datetime(errors='coerce')` to order master frames:
/// `YYYY-MM-DD` with an optional `T`/space time and fractional seconds.
/// Anything else is `NaT`.
///
/// No fixture reaches this — it needs two masters in one hardware group — so
/// it is deliberately narrow rather than a speculative reimplementation of
/// pandas' parser. Widen it against a real dataset, not by guessing.
fn parse_iso_datetime(s: &str) -> Option<(i32, u32, u32, u32, u32, u32, u32)> {
    let s = s.trim();
    let (date, time) = match s.split_once(['T', ' ']) {
        Some((d, t)) => (d, t),
        None => (s, ""),
    };
    let mut dparts = date.split('-');
    let y: i32 = dparts.next()?.parse().ok()?;
    let mo: u32 = dparts.next()?.parse().ok()?;
    let d: u32 = dparts.next()?.parse().ok()?;
    if dparts.next().is_some() {
        return None;
    }
    if time.is_empty() {
        return Some((y, mo, d, 0, 0, 0, 0));
    }
    let mut tparts = time.split(':');
    let h: u32 = tparts.next()?.parse().ok()?;
    let mi: u32 = tparts.next().unwrap_or("0").parse().ok()?;
    let sec_text = tparts.next().unwrap_or("0");
    let (sec, frac) = match sec_text.split_once('.') {
        Some((a, b)) => {
            let micros: String = format!("{b:0<6}").chars().take(6).collect();
            (a.parse::<u32>().ok()?, micros.parse::<u32>().ok()?)
        }
        None => (sec_text.parse::<u32>().ok()?, 0),
    };
    Some((y, mo, d, h, mi, sec, frac))
}

/// Stage 6 — collapse the wild variety of `IMAGETYP` spellings onto the
/// normalised set.
fn stage6_normalize_image_type(df: &mut Table) {
    if !df.has_column(col::IMAGE_TYPE) {
        return;
    }

    // `sorted(type_map.items(), key=lambda x: len(x[0]), reverse=True)` --
    // stable, so equal-length keywords keep the dict's insertion order. Spelt
    // out here in the resulting order rather than re-derived, since that
    // order is the whole point: longest first, so 'DARK' cannot claim a
    // 'DARKFLAT'.
    use crate::constants::image_type as it;
    const TYPE_MAP: &[(&str, &str)] = &[
        ("MASTERDARKFLAT", it::MASTER_DARKFLAT),
        ("MASTER FLAT", it::MASTER_FLAT),
        ("MASTER DARK", it::MASTER_DARK),
        ("MASTER BIAS", it::MASTER_BIAS),
        ("MASTERFLAT", it::MASTER_FLAT),
        ("MASTERDARK", it::MASTER_DARK),
        ("MASTERBIAS", it::MASTER_BIAS),
        ("DARK FLAT", it::DARK_FLAT),
        ("DARKFLAT", it::DARK_FLAT),
        ("LIGHT", it::LIGHT),
        ("FLAT", it::FLAT),
        ("DARK", it::DARK),
        ("BIAS", it::BIAS),
    ];

    // Matching is against a frozen snapshot, and each row is assigned once.
    let original: Vec<String> = df
        .column(col::IMAGE_TYPE)
        .unwrap()
        .cells
        .iter()
        .map(|c| astype_str(c).to_uppercase())
        .collect();
    let mut assigned = vec![false; df.n_rows];
    let mut out: Vec<Cell> = df.column(col::IMAGE_TYPE).unwrap().cells.clone();

    for (keyword, normalized) in TYPE_MAP {
        for i in 0..original.len() {
            if !assigned[i] && original[i].contains(keyword) {
                out[i] = Cell::Str((*normalized).to_string());
                assigned[i] = true;
            }
        }
    }

    df.set_column(
        col::IMAGE_TYPE,
        Column {
            name: col::IMAGE_TYPE.to_string(),
            dtype: DType::Str,
            cells: out,
        },
    );
}

/// The core columns, their defaults, and how each is hardened.
enum Harden {
    /// `to_numeric(coerce).fillna(default).astype(float)`
    Float(f64),
    /// `exposure`: as above but with a pandas `.round(2)` in the middle.
    Exposure,
    /// `gain`: `.round()` then `astype(int)`.
    Gain,
    /// `number`: `fillna(1).astype(int)`, preserving master sub-counts.
    Number,
    /// `site`: `astype(str).replace('nan', default)`.
    Site,
    /// Present in the core list only so it is created when missing; an
    /// existing column is left exactly as found.
    AsFound(Cell),
}

/// Stage 7 — make sure the core columns exist and carry the expected type.
fn stage7_harden(df: &mut Table) {
    use Harden::*;
    let core: &[(&str, Cell, Harden)] = &[
        (col::GAIN, Cell::Int(0), Gain),
        (col::EGAIN, Cell::Float(1.0), Float(1.0)),
        (col::DURATION, Cell::Float(0.0), Exposure),
        (col::SENSOR_COOLING, Cell::Float(-10.0), Float(-10.0)),
        (col::FOCAL_LENGTH, Cell::Int(500), Float(500.0)),
        (col::F_NUMBER, Cell::Float(5.0), Float(5.0)),
        (col::PIXEL_SIZE, Cell::Float(3.76), Float(3.76)),
        (col::SITE_LAT, Cell::Float(0.0), Float(0.0)),
        (col::SITE_LONG, Cell::Float(0.0), Float(0.0)),
        (col::BORTLE, Cell::Float(4.0), Float(4.0)),
        (col::MEAN_SQM, Cell::Float(21.0), Float(21.0)),
        (col::TEMPERATURE, Cell::Float(20.0), AsFound(Cell::Float(20.0))),
        (
            col::TARGET,
            Cell::Str("Unknown".into()),
            AsFound(Cell::Str("Unknown".into())),
        ),
        (
            col::FILTER_NAME,
            Cell::Str("No Filter".into()),
            AsFound(Cell::Str("No Filter".into())),
        ),
        (col::SITE_NAME, Cell::Str("Unknown Site".into()), Site),
        (col::BINNING, Cell::Int(1), Float(1.0)),
        (col::HFR, Cell::Float(1.0), Float(1.0)),
        (col::MEAN_FWHM, Cell::Float(0.0), Float(0.0)),
        (col::IMSCALE, Cell::Float(1.0), Float(1.0)),
        (col::NUMBER, Cell::Int(1), Number),
        ("darks", Cell::Int(0), AsFound(Cell::Int(0))),
        ("flats", Cell::Int(0), AsFound(Cell::Int(0))),
        ("flatDarks", Cell::Int(0), AsFound(Cell::Int(0))),
        ("bias", Cell::Int(0), AsFound(Cell::Int(0))),
    ];

    for (name, missing_default, how) in core {
        let Some(column) = df.column(name) else {
            df.set_scalar(name, missing_default.clone());
            continue;
        };

        let hardened = match how {
            AsFound(_) => continue,
            Float(default) => Column {
                name: name.to_string(),
                dtype: DType::Float,
                cells: column
                    .cells
                    .iter()
                    .map(|c| Cell::Float(to_numeric(c).unwrap_or(*default)))
                    .collect(),
            },
            Exposure => Column {
                name: name.to_string(),
                dtype: DType::Float,
                cells: column
                    .cells
                    .iter()
                    .map(|c| Cell::Float(numpy_round(to_numeric(c).unwrap_or(0.0), 2)))
                    .collect(),
            },
            Gain => Column {
                name: name.to_string(),
                dtype: DType::Int,
                cells: column
                    .cells
                    .iter()
                    // `.round()` then `astype(int)`: the cast truncates
                    // towards zero, which is exact on an already-rounded
                    // value.
                    .map(|c| Cell::Int(numpy_round(to_numeric(c).unwrap_or(0.0), 0) as i64))
                    .collect(),
            },
            Number => Column {
                name: name.to_string(),
                dtype: DType::Int,
                cells: column
                    .cells
                    .iter()
                    .map(|c| Cell::Int(to_numeric(c).unwrap_or(1.0) as i64))
                    .collect(),
            },
            Site => Column {
                name: name.to_string(),
                dtype: DType::Str,
                cells: column
                    .cells
                    .iter()
                    .map(|c| {
                        let s = astype_str(c);
                        Cell::Str(if s == "nan" {
                            "Unknown Site".to_string()
                        } else {
                            s
                        })
                    })
                    .collect(),
            },
        };
        df.set_column(name, hardened);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigFile;

    fn config(text: &str) -> AppConfig {
        AppConfig::from_config(&ConfigFile::parse_str(text).unwrap()).unwrap()
    }

    fn types(t: &Table) -> Vec<String> {
        t.column(col::IMAGE_TYPE)
            .unwrap()
            .cells
            .iter()
            .map(astype_str)
            .collect()
    }

    #[test]
    fn override_maps_a_hardware_key_onto_an_internal_one_and_drops_the_source() {
        let raw = Table::parse_str("IMAGETYP,EXPTIME\nLIGHT,600\n").unwrap();
        let mut df = raw.clone();
        stage1_hardware_overrides(&mut df, &config("[override]\nEXPOSURE = EXPTIME\n")).unwrap();
        assert!(df.column("EXPTIME").is_none());
        assert_eq!(df.column("EXPOSURE").unwrap().cells[0], Cell::Int(600));
    }

    #[test]
    fn a_second_override_candidate_only_fills_the_first_ones_gaps() {
        let raw = Table::parse_str("A,B\n1.0,9.0\n,8.0\n").unwrap();
        let mut df = raw.clone();
        stage1_hardware_overrides(&mut df, &config("[override]\nSQM = A, B\n")).unwrap();
        let sqm = df.column("SQM").unwrap();
        assert_eq!(sqm.cells, vec![Cell::Float(1.0), Cell::Float(8.0)]);
    }

    #[test]
    fn duplicate_names_after_lowercasing_are_coalesced_and_sorted() {
        // 'z' is unique, 'a' is duplicated -- the whole frame comes back
        // sorted, which is the behaviour of the groupby this replaced.
        let mut df = Table::parse_str("Z,A,a\n1,,5\n2,4,6\n").unwrap();
        stage2_lowercase_and_coalesce(&mut df).unwrap();
        assert_eq!(
            df.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["a", "z"]
        );
        assert_eq!(
            df.column("a").unwrap().cells,
            vec![Cell::Float(5.0), Cell::Float(4.0)]
        );
    }

    #[test]
    fn without_duplicates_stage2_leaves_column_order_alone() {
        let mut df = Table::parse_str("Z,A\n1,2\n").unwrap();
        stage2_lowercase_and_coalesce(&mut df).unwrap();
        assert_eq!(
            df.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["z", "a"]
        );
    }

    #[test]
    fn defaults_inject_strings_and_only_where_the_column_is_missing() {
        let mut df = Table::parse_str("gain\n100\n").unwrap();
        stage3_defaults(&mut df, &config("[defaults]\nGAIN = -1\nROTANTANG = 0\n")).unwrap();
        assert_eq!(df.column("gain").unwrap().cells[0], Cell::Int(100));
        let rot = df.column("rotantang").unwrap();
        assert_eq!(rot.dtype, DType::Str);
        assert_eq!(rot.cells[0], Cell::Str("0".into()));
    }

    #[test]
    fn equipment_overrides_win_over_the_found_value() {
        let mut df = Table::parse_str("focname\nEAF\n").unwrap();
        stage3b_equipment_overrides(&mut df, &config("[equipmentoverrides]\nFOCNAME = ZWO EAF\n"));
        assert_eq!(df.column("focname").unwrap().cells[0], Cell::Str("ZWO EAF".into()));
    }

    #[test]
    fn master_lights_and_missing_types_are_dropped() {
        let df = Table::parse_str("imagetyp,n\nLIGHT,1\nMASTERLIGHT,2\nMaster Light,3\n,4\n")
            .unwrap();
        let out = stage4_initial_filter(df);
        assert_eq!(out.n_rows, 1);
        assert_eq!(out.column("n").unwrap().cells[0], Cell::Int(1));
    }

    #[test]
    fn a_master_preempts_the_raw_frames_of_its_own_group() {
        let mut df = Table::parse_str(
            "imagetyp,gain,egain,xbinning,exposure,filter,n\n\
             LIGHT,100,0.25,1,600,Ha,1\n\
             DARK,100,0.25,1,600,Ha,2\n\
             DARK,100,0.25,1,600,Ha,3\n\
             MASTERDARK,100,0.25,1,600,Ha,4\n",
        )
        .unwrap();
        let out = stage5_master_preference(&mut df).unwrap();
        // Light first, then the single surviving master.
        assert_eq!(
            out.column("n").unwrap().cells,
            vec![Cell::Int(1), Cell::Int(4)]
        );
    }

    #[test]
    fn without_a_master_every_raw_frame_survives_but_the_rows_still_reorder() {
        let mut df = Table::parse_str(
            "imagetyp,gain,egain,xbinning,exposure,filter,n\n\
             DARK,100,0.25,1,600,Ha,1\n\
             LIGHT,100,0.25,1,600,Ha,2\n",
        )
        .unwrap();
        let out = stage5_master_preference(&mut df).unwrap();
        assert_eq!(
            out.column("n").unwrap().cells,
            vec![Cell::Int(2), Cell::Int(1)]
        );
    }

    #[test]
    fn no_calibration_frames_means_no_reordering_at_all() {
        let mut df = Table::parse_str("imagetyp,n\nLIGHT,1\nLIGHT,2\n").unwrap();
        let out = stage5_master_preference(&mut df).unwrap();
        assert_eq!(
            out.column("n").unwrap().cells,
            vec![Cell::Int(1), Cell::Int(2)]
        );
    }

    #[test]
    fn longest_keyword_wins_and_a_row_is_only_rewritten_once() {
        // A13: without the frozen snapshot, 'DARK' re-matches the
        // 'MASTERDARK' the earlier keyword just wrote.
        let mut df = Table::parse_str(
            "imagetyp\nMASTER DARK\nMASTERDARKFLAT\nDARK FLAT\nLight Frame\nBias Frame\n",
        )
        .unwrap();
        let df2 = stage4_initial_filter(df.clone());
        df = df2;
        stage6_normalize_image_type(&mut df);
        assert_eq!(
            types(&df),
            vec!["MASTERDARK", "MASTERDARKFLAT", "DARKFLAT", "LIGHT", "BIAS"]
        );
    }

    #[test]
    fn hardening_is_asymmetric_per_column() {
        let mut df = Table::parse_str(
            "gain,exposure,number,site,xbinning,foctemp\n\
             100.6,600.005,12,,2,20.5\n",
        )
        .unwrap();
        stage7_harden(&mut df);
        assert_eq!(df.column("gain").unwrap().dtype, DType::Int);
        assert_eq!(df.column("gain").unwrap().cells[0], Cell::Int(101));
        assert_eq!(df.column("exposure").unwrap().dtype, DType::Float);
        assert_eq!(df.column("number").unwrap().cells[0], Cell::Int(12));
        // A missing site becomes the default, not the string 'nan'.
        assert_eq!(
            df.column("site").unwrap().cells[0],
            Cell::Str("Unknown Site".into())
        );
        // xbinning is cast to float even though it arrived as an integer...
        assert_eq!(df.column("xbinning").unwrap().dtype, DType::Float);
        // ...while foctemp, also a core column, is left exactly as found.
        assert_eq!(df.column("foctemp").unwrap().cells[0], Cell::Float(20.5));
        // The counters are appended, in list order.
        let tail: Vec<&str> = df
            .columns
            .iter()
            .rev()
            .take(4)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(tail, vec!["bias", "flatDarks", "flats", "darks"]);
    }

    #[test]
    fn group_keys_sort_like_python_tuples() {
        let a = GroupKey {
            base_type: "DARK".into(),
            gain: 100,
            egain: "0.25".into(),
            binning: "1".into(),
            tail: "600.00".into(),
        };
        let mut b = a.clone();
        b.gain = 20;
        assert!(b < a); // int compares numerically, not as text
    }
}

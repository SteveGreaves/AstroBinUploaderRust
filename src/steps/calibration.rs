//! `CalibrationMatcherStep` — which darks, flats, bias and flat-darks belong
//! to each light frame.
//!
//! Three things happen here, and only the middle one changes the row count:
//!
//! 1. A **hybrid gain key** per row: the electronic gain when it is genuinely
//!    set (`E_0.25`), otherwise the linear gain (`G_100`). Rounding EGAIN to
//!    two decimals is what lets a master's stored gain meet a raw sub's.
//! 2. **Light-frame authority**: a calibration frame whose (gain, binning) —
//!    plus filter, for flats — matches no light frame is discarded outright.
//!    This is the step that drops rows: 214 of 1693 flats in the
//!    `sh2101_calib` fixture, all of them shot at a gain no light uses.
//! 3. **Counting**, with the same master preference as stage 5 of
//!    normalisation: if a master exists among the candidates, exactly one
//!    master is counted and the raw subs it was built from are ignored.
//!
//! The result is `concat([lights, cals])`, so the rows come out lights-first
//! again.

use anyhow::{bail, Result};

use crate::constants as col;
use crate::constants::image_type::LIGHT;
use crate::steps::{astype_str, python_float};
use crate::table::{Cell, Column, DType, Table};

/// EGAIN this close to `NormalizeHeadersStep`'s 1.0 default is treated as
/// unset: a reading of exactly 1.0 e/ADU is far likelier to be the
/// placeholder than a real measurement, so the key falls back to linear gain.
const EGAIN_UNSET_TOLERANCE: f64 = 0.0001;

pub fn execute(table: &Table) -> Result<Table> {
    let mut df = table.clone();
    if df.n_rows == 0 {
        return Ok(df);
    }

    // --- Stage 1: hybrid handshake -----------------------------------------
    let gain_match: Vec<String> = (0..df.n_rows).map(|i| hybrid_key(&df, i)).collect();
    df.set_column(
        col::GAIN_MATCH,
        Column {
            name: col::GAIN_MATCH.to_string(),
            dtype: DType::Str,
            cells: gain_match.iter().map(|s| Cell::Str(s.clone())).collect(),
        },
    );

    let Some(itype_col) = df.column(col::IMAGE_TYPE) else {
        bail!("calibration matching needs an '{}' column", col::IMAGE_TYPE);
    };
    let types: Vec<String> = itype_col.cells.iter().map(astype_str).collect();
    let upper: Vec<String> = types.iter().map(|s| s.to_uppercase()).collect();
    let is_light: Vec<bool> = types.iter().map(|t| t == LIGHT).collect();

    // Binning and exposure are compared for exact equality, as pandas does;
    // the bit pattern is the honest way to say that.
    let binning: Vec<u64> = numeric_bits(&df, col::BINNING)?;
    let exposure: Vec<u64> = numeric_bits(&df, col::DURATION)?;
    let filters: Vec<Option<String>> = str_lower(&df, col::FILTER_NAME);
    // The light side stringifies before lowering (`str(row[...]).lower()`),
    // so a null becomes "nan" there while `.str.lower()` on the frame leaves
    // it null. Both spellings are needed.
    let filters_str: Vec<String> = match df.column(col::FILTER_NAME) {
        Some(c) => c.cells.iter().map(|v| astype_str(v).to_lowercase()).collect(),
        None => vec!["none".to_string(); df.n_rows],
    };

    // --- Stage 2: light-frame authority ------------------------------------
    let light_rows: Vec<usize> = (0..df.n_rows).filter(|&i| is_light[i]).collect();
    let mut keep: Vec<usize> = (0..df.n_rows).collect();

    if !light_rows.is_empty() {
        let dark_bias_anchors: std::collections::HashSet<(&str, u64)> = light_rows
            .iter()
            .map(|&i| (gain_match[i].as_str(), binning[i]))
            .collect();
        let flat_anchors: std::collections::HashSet<(String, &str, u64)> = light_rows
            .iter()
            .map(|&i| (filters_str[i].clone(), gain_match[i].as_str(), binning[i]))
            .collect();

        keep = (0..df.n_rows)
            .filter(|&i| {
                let t = &upper[i];
                if t == LIGHT {
                    return true;
                }
                if t.contains("FLAT") && !t.contains("DARK") {
                    return flat_anchors.contains(&(
                        filters_str[i].clone(),
                        gain_match[i].as_str(),
                        binning[i],
                    ));
                }
                if t.contains("DARK") || t.contains("BIAS") {
                    return dark_bias_anchors.contains(&(gain_match[i].as_str(), binning[i]));
                }
                true
            })
            .collect();
    }

    // --- Stage 3/4/5: segment, count, reintegrate --------------------------
    let lights: Vec<usize> = keep.iter().copied().filter(|&i| is_light[i]).collect();
    let cals: Vec<usize> = keep.iter().copied().filter(|&i| !is_light[i]).collect();

    if lights.is_empty() {
        return Ok(df.take_rows(&keep));
    }

    let number: Vec<i64> = match df.column(col::NUMBER) {
        Some(c) => c
            .cells
            .iter()
            .map(|v| match v {
                Cell::Int(n) => *n,
                other => crate::steps::to_numeric(other).unwrap_or(0.0) as i64,
            })
            .collect(),
        None => vec![1; df.n_rows],
    };
    let dates: Vec<String> = match df.column(col::DATE_OBS) {
        Some(c) => c.cells.iter().map(astype_str).collect(),
        None => vec![String::new(); df.n_rows],
    };

    let mut counts: Vec<[i64; 4]> = vec![[0; 4]; df.n_rows]; // darks, flats, flatDarks, bias
    for &l in &lights {
        let same_hardware =
            |c: usize| gain_match[c] == gain_match[l] && binning[c] == binning[l];
        let same_filter = |c: usize| filters[c].as_deref() == Some(filters_str[l].as_str());

        let darks: Vec<usize> = cals
            .iter()
            .copied()
            .filter(|&c| {
                upper[c].contains("DARK")
                    && !upper[c].contains("FLAT")
                    && same_hardware(c)
                    && exposure[c] == exposure[l]
            })
            .collect();
        let bias: Vec<usize> = cals
            .iter()
            .copied()
            .filter(|&c| upper[c].contains("BIAS") && same_hardware(c))
            .collect();
        let flats: Vec<usize> = cals
            .iter()
            .copied()
            .filter(|&c| {
                upper[c].contains("FLAT")
                    && !upper[c].contains("DARK")
                    && same_filter(c)
                    && same_hardware(c)
            })
            .collect();
        let flat_darks: Vec<usize> = cals
            .iter()
            .copied()
            .filter(|&c| upper[c].contains("DARKFLAT") && same_filter(c) && same_hardware(c))
            .collect();

        counts[l] = [
            resolve_count(&darks, &upper, &number, &dates),
            resolve_count(&flats, &upper, &number, &dates),
            resolve_count(&flat_darks, &upper, &number, &dates),
            resolve_count(&bias, &upper, &number, &dates),
        ];
    }

    // `lights[col] = 0` resets the counters for lights only; the calibration
    // frames keep whatever Stage 7 left there.
    for (slot, name) in ["darks", "flats", "flatDarks", "bias"].iter().enumerate() {
        let mut cells = match df.column(name) {
            Some(c) => c.cells.clone(),
            None => vec![Cell::Int(0); df.n_rows],
        };
        for &l in &lights {
            cells[l] = Cell::Int(counts[l][slot]);
        }
        df.set_column(
            name,
            Column {
                name: name.to_string(),
                dtype: DType::Int,
                cells,
            },
        );
    }

    let mut order = lights;
    order.extend(cals);
    Ok(df.take_rows(&order))
}

/// `create_hybrid_key`: `E_<egain to 2dp>` when EGAIN is meaningfully set,
/// else `G_<rounded linear gain>`, else `G_0`.
///
/// A NaN EGAIN is not an error — `abs(nan - 1.0) > tol` is simply false — so
/// it falls through to the gain branch rather than being caught.
fn hybrid_key(df: &Table, row: usize) -> String {
    if let Some(c) = df.column(col::EGAIN) {
        if let Ok(egain) = python_float(&c.cells[row]) {
            if (egain - 1.0).abs() > EGAIN_UNSET_TOLERANCE {
                return format!("E_{egain:.2}");
            }
        }
    }
    match df.column(col::GAIN).map(|c| python_float(&c.cells[row])) {
        Some(Ok(g)) if g.is_finite() => format!("G_{}", g.round_ties_even() as i64),
        _ => "G_0".to_string(),
    }
}

/// Sum the `number` column over the candidates, preferring a master.
///
/// With a master present exactly one frame is counted — the most recent by
/// `DATE-OBS` when several exist — because a master and the raw subs it was
/// built from describe the same photons.
fn resolve_count(
    candidates: &[usize],
    upper: &[String],
    number: &[i64],
    dates: &[String],
) -> i64 {
    if candidates.is_empty() {
        return 0;
    }
    let masters: Vec<usize> = candidates
        .iter()
        .copied()
        .filter(|&i| upper[i].contains("MASTER"))
        .collect();

    let final_set: Vec<usize> = match masters.len() {
        0 => candidates.to_vec(),
        1 => masters,
        _ => {
            let latest = masters
                .iter()
                .filter_map(|&i| {
                    crate::steps::normalize::parse_iso_datetime(&dates[i]).map(|d| (d, i))
                })
                .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
            vec![latest.map_or(masters[0], |(_, i)| i)]
        }
    };
    final_set.iter().map(|&i| number[i]).sum()
}

/// A numeric column's bit patterns, for the exact-equality comparisons pandas
/// does on `xbinning` and `exposure`.
fn numeric_bits(df: &Table, name: &str) -> Result<Vec<u64>> {
    let Some(c) = df.column(name) else {
        bail!("calibration matching needs a '{name}' column");
    };
    Ok(c.cells
        .iter()
        .map(|v| match crate::steps::to_numeric(v) {
            Some(x) => x.to_bits(),
            // NaN never equals anything in pandas either; a distinct sentinel
            // keeps that true here.
            None => u64::MAX,
        })
        .collect())
}

/// `series.str.lower()` — `None` for anything that is not a string, which is
/// what the `.str` accessor yields and what makes the comparison fail rather
/// than match.
fn str_lower(df: &Table, name: &str) -> Vec<Option<String>> {
    match df.column(name) {
        None => vec![None; df.n_rows],
        Some(c) => c
            .cells
            .iter()
            .map(|v| match v {
                Cell::Str(s) => Some(s.to_lowercase()),
                _ => None,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn egain_drives_the_key_unless_it_is_the_unset_default() {
        let df = Table::parse_str("egain,gain\n0.246657,100\n1.0,100\n1.00005,100\n").unwrap();
        assert_eq!(hybrid_key(&df, 0), "E_0.25");
        assert_eq!(hybrid_key(&df, 1), "G_100"); // exactly the 1.0 default
        assert_eq!(hybrid_key(&df, 2), "G_100"); // within tolerance of it
    }

    #[test]
    fn a_missing_gain_falls_all_the_way_back() {
        let df = Table::parse_str("egain,gain\n1.0,\n").unwrap();
        assert_eq!(hybrid_key(&df, 0), "G_0");
    }

    #[test]
    fn a_flat_at_a_gain_no_light_uses_is_discarded() {
        let df = Table::parse_str(
            "imagetyp,egain,gain,xbinning,exposure,filter,number,date-obs\n\
             LIGHT,0.25,100,1.0,600.0,Ha,1,2023-07-06T02:00:00\n\
             FLAT,0.25,100,1.0,3.0,Ha,1,2023-07-06T12:00:00\n\
             FLAT,0.78,1,1.0,3.0,Ha,1,2023-07-06T12:00:00\n",
        )
        .unwrap();
        let out = execute(&df).unwrap();
        assert_eq!(out.n_rows, 2);
        // The surviving light carries the flat's count; rows come out
        // lights-first.
        assert_eq!(out.column("flats").unwrap().cells[0], Cell::Int(1));
        assert_eq!(out.column("gain_match").unwrap().cells[0], Cell::Str("E_0.25".into()));
    }

    #[test]
    fn a_master_is_counted_instead_of_the_raws_it_replaces() {
        let df = Table::parse_str(
            "imagetyp,egain,gain,xbinning,exposure,filter,number,date-obs\n\
             LIGHT,0.25,100,1.0,600.0,Ha,1,2023-07-06T02:00:00\n\
             DARK,0.25,100,1.0,600.0,Ha,1,2023-07-06T12:00:00\n\
             DARK,0.25,100,1.0,600.0,Ha,1,2023-07-06T12:00:00\n\
             MASTERDARK,0.25,100,1.0,600.0,Ha,30,2023-07-07T12:00:00\n",
        )
        .unwrap();
        let out = execute(&df).unwrap();
        // 30 from the master, not 32.
        assert_eq!(out.column("darks").unwrap().cells[0], Cell::Int(30));
    }

    #[test]
    fn darks_ignore_the_filter_but_not_the_exposure() {
        let df = Table::parse_str(
            "imagetyp,egain,gain,xbinning,exposure,filter,number,date-obs\n\
             LIGHT,0.25,100,1.0,600.0,Ha,1,2023-07-06T02:00:00\n\
             DARK,0.25,100,1.0,600.0,OIII,7,2023-07-06T12:00:00\n\
             DARK,0.25,100,1.0,300.0,Ha,5,2023-07-06T12:00:00\n",
        )
        .unwrap();
        let out = execute(&df).unwrap();
        assert_eq!(out.column("darks").unwrap().cells[0], Cell::Int(7));
    }
}

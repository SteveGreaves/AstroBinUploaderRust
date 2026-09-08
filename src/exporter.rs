//! `engine/exporter.py` — the acquisition CSV and the session summary file.
//!
//! Two artifacts come out of here and they share one frame:
//!
//! 1. `<basename>_acquisition.csv` — LIGHT rows only, sixteen columns whose
//!    order is the literal key order of `exporter.py`'s `mapping` dict (not
//!    the aggregation rules' order, which is unrelated), rounded and written
//!    with `to_csv(index=False)`.
//! 2. `<basename>_session_summary.txt` — `reports::generate_full_summary`
//!    with that same frame's `to_string(index=False)` appended.
//!
//! The rounding is `DataFrame.round(dict)`, i.e. **numpy's** multiply–rint–
//! divide, not the builtin `round` that `optical.py` uses. PORT_PLAN.md
//! hazard 3: the two disagree at decimal boundaries and both are live in this
//! codebase.

use anyhow::Result;
use std::path::Path;

use crate::constants as col;
use crate::constants::image_type::LIGHT;
use crate::numeric::numpy_round;
use crate::pandas_fmt::to_string_index_false;
use crate::steps::astype_str;
use crate::table::{Cell, Column, DType, Table};

/// `mapping` from `exporter.py`, in source order. `(aggregated column, CSV
/// header)`. The order is the column order of both output artifacts.
const MAPPING: &[(&str, &str)] = &[
    ("session_date", "date"),
    ("filter_code", "filter"),
    (col::NUMBER, "number"),
    (col::DURATION, "duration"),
    (col::BINNING, "binning"),
    (col::GAIN, "gain"),
    (col::SENSOR_COOLING, "sensorCooling"),
    (col::F_NUMBER, "fNumber"),
    ("darks", "darks"),
    ("flats", "flats"),
    ("flatDarks", "flatDarks"),
    ("bias", "bias"),
    (col::BORTLE, "bortle"),
    (col::MEAN_SQM, "meanSqm"),
    (col::MEAN_FWHM, "meanFwhm"),
    (col::TEMPERATURE, "temperature"),
];

/// `acq_df.round({...})`, in `exporter.py`'s order.
const ROUNDING: &[(&str, u32)] = &[
    ("duration", 2),
    ("sensorCooling", 0),
    ("fNumber", 2),
    ("meanSqm", 2),
    ("meanFwhm", 2),
    ("temperature", 2),
];

/// The LIGHT-only, renamed, rounded frame both artifacts are built from.
///
/// `filter_code` is already on the aggregated frame — `AggregationStep`'s
/// stage 4 builds it — so `exporter.py`'s `if 'filter_code' not in
/// acq_source.columns` fallback is dead and is not reproduced.
pub fn build_acquisition(agg: &Table) -> Result<Table> {
    let Some(itype) = agg.column(col::IMAGE_TYPE) else {
        anyhow::bail!("the aggregated frame has no '{}' column", col::IMAGE_TYPE);
    };
    let rows: Vec<usize> = (0..agg.n_rows)
        .filter(|&i| matches!(&itype.cells[i], Cell::Str(s) if s == LIGHT))
        .collect();
    let lights = agg.take_rows(&rows);

    // B12 upstream: name every missing column at once rather than raising a
    // bare KeyError on the first.
    let missing: Vec<&str> = MAPPING
        .iter()
        .map(|(src, _)| *src)
        .filter(|src| !lights.has_column(src))
        .collect();
    if !missing.is_empty() {
        anyhow::bail!(
            "Aggregated data is missing expected column(s) {missing:?} required for the \
             acquisition CSV. This usually means a core column was renamed or an \
             AggregationStep reduction rule was dropped -- check src/steps/aggregate.rs's \
             `rules` list against this module's `MAPPING`."
        );
    }

    let mut acq = Table {
        columns: MAPPING
            .iter()
            .map(|(src, dst)| {
                let c = lights.column(src).expect("checked above");
                Column {
                    name: (*dst).to_string(),
                    dtype: c.dtype,
                    cells: c.cells.clone(),
                }
            })
            .collect(),
        n_rows: lights.n_rows,
    };

    for (name, ndigits) in ROUNDING {
        let Some(c) = acq.column_mut(name) else { continue };
        // `.round()` on a non-float column is a no-op in pandas (int64 is
        // already integral, and an object column is skipped entirely).
        if c.dtype != DType::Float {
            continue;
        }
        for cell in &mut c.cells {
            if let Cell::Float(v) = cell {
                *cell = Cell::Float(numpy_round(*v, *ndigits));
            }
        }
    }

    Ok(acq)
}

/// `DataFrame.to_csv(index=False)`: `\n` terminators, a trailing newline, and
/// `str()` on every value. A null writes an empty field.
pub fn to_csv(t: &Table) -> String {
    let mut out = String::new();
    let names: Vec<&str> = t.columns.iter().map(|c| c.name.as_str()).collect();
    out.push_str(&names.join(","));
    out.push('\n');
    for r in 0..t.n_rows {
        let fields: Vec<String> = t
            .columns
            .iter()
            .map(|c| match &c.cells[r] {
                Cell::Null => String::new(),
                other => quote(&astype_str(other)),
            })
            .collect();
        out.push_str(&fields.join(","));
        out.push('\n');
    }
    out
}

/// `csv.QUOTE_MINIMAL`: quote only when the field carries a delimiter, a
/// quote, or a line break.
fn quote(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// `Exporter.export`. Writes both files and returns the summary text, which
/// the Python side also prints to stdout.
pub fn export(
    agg: &Table,
    total_scanned: usize,
    basename: &str,
    out_dir: &Path,
    now: &str,
) -> Result<Option<String>> {
    if agg.n_rows == 0 {
        // `self.logger.warning("Export requested but aggregated data is
        // empty.")` and no files at all.
        eprintln!("Export requested but aggregated data is empty.");
        return Ok(None);
    }

    let acq = build_acquisition(agg)?;
    std::fs::write(
        out_dir.join(format!("{basename}_acquisition.csv")),
        to_csv(&acq),
    )?;

    let mut summary = crate::reports::generate_full_summary(agg, total_scanned, now);
    let df_string = to_string_index_false(&acq).replace('\n', "\n ");
    summary.push_str(&format!("\n{basename}_acquisition.csv\n\n {df_string}\n"));

    std::fs::write(
        out_dir.join(format!("{basename}_session_summary.txt")),
        &summary,
    )?;
    Ok(Some(summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_csv_renders_ints_as_ints_and_whole_floats_with_a_decimal_point() {
        let t = Table {
            columns: vec![
                Column {
                    name: "gain".into(),
                    dtype: DType::Int,
                    cells: vec![Cell::Int(100)],
                },
                Column {
                    name: "duration".into(),
                    dtype: DType::Float,
                    cells: vec![Cell::Float(600.0)],
                },
                Column {
                    name: "date".into(),
                    dtype: DType::Str,
                    cells: vec![Cell::Str("2023-07-05".into())],
                },
            ],
            n_rows: 1,
        };
        assert_eq!(to_csv(&t), "gain,duration,date\n100,600.0,2023-07-05\n");
    }

    /// A minimal aggregated frame: the sixteen source columns `MAPPING`
    /// names, one LIGHT row and one FLAT row.
    fn aggregated(exposure: &str, ccd_temp: &str) -> Table {
        let header = "imagetyp,session_date,filter_code,number,exposure,xbinning,gain,\
                      ccd-temp,focratio,darks,flats,flatDarks,bias,bortle,sqm,fwhm,foctemp";
        Table::parse_str(&format!(
            "{header}\n\
             LIGHT,2023-07-05,4663,7,{exposure},1.0,100,{ccd_temp},5.4,0,0,0,0,4.0,21.0,4.68,8.97\n\
             FLAT,2023-07-05,4663,10,0.5,1.0,100,-10.0,5.4,0,0,0,0,4.0,21.0,4.68,8.97\n"
        ))
        .unwrap()
    }

    #[test]
    fn only_light_rows_reach_the_csv_and_the_columns_come_out_in_mapping_order() {
        let acq = build_acquisition(&aggregated("600.0", "-10.0")).unwrap();
        assert_eq!(acq.n_rows, 1);
        let names: Vec<&str> = acq.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, MAPPING.iter().map(|(_, d)| *d).collect::<Vec<_>>());
        assert_eq!(
            to_csv(&acq).lines().next().unwrap(),
            "date,filter,number,duration,binning,gain,sensorCooling,fNumber,darks,flats,\
             flatDarks,bias,bortle,meanSqm,meanFwhm,temperature"
        );
    }

    /// `acq_df.round()` is pandas', i.e. numpy's multiply-rint-divide — not
    /// the builtin `round` `optical.py` uses. The two disagree at decimal
    /// boundaries (PORT_PLAN.md hazard 3) and both are live in this codebase,
    /// so reaching for the wrong one here is a silent one-cent error.
    #[test]
    fn the_csv_rounding_is_numpys_not_the_python_builtins() {
        let acq = build_acquisition(&aggregated("2.675", "-10.0")).unwrap();
        assert_eq!(
            acq.column("duration").unwrap().cells[0],
            Cell::Float(2.68),
            "numpy rounds 2.675 up; the builtin gives 2.67"
        );
        assert_ne!(
            acq.column("duration").unwrap().cells[0],
            Cell::Float(crate::numeric::python_round(2.675, 2))
        );
    }

    #[test]
    fn sensor_cooling_rounds_to_zero_places_and_stays_a_float() {
        let acq = build_acquisition(&aggregated("600.0", "-10.4")).unwrap();
        assert_eq!(acq.column("sensorCooling").unwrap().cells[0], Cell::Float(-10.0));
        assert_eq!(acq.column("sensorCooling").unwrap().dtype, DType::Float);
    }

    #[test]
    fn a_missing_source_column_names_every_one_that_is_missing() {
        let mut agg = aggregated("600.0", "-10.0");
        agg.drop_column("focratio");
        agg.drop_column("bortle");
        let err = build_acquisition(&agg).unwrap_err().to_string();
        assert!(err.contains("focratio"), "{err}");
        assert!(err.contains("bortle"), "{err}");
    }

    #[test]
    fn a_null_writes_an_empty_field_and_a_comma_gets_quoted() {
        let t = Table {
            columns: vec![
                Column {
                    name: "a".into(),
                    dtype: DType::Float,
                    cells: vec![Cell::Null],
                },
                Column {
                    name: "b".into(),
                    dtype: DType::Str,
                    cells: vec![Cell::Str("x,y".into())],
                },
            ],
            n_rows: 1,
        };
        assert_eq!(to_csv(&t), "a,b\n,\"x,y\"\n");
    }
}

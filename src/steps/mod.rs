//! The pipeline steps, in order, plus the pandas-semantics helpers they share.
//!
//! Each step is a pure function `Table -> Table` rather than a mutation of a
//! shared state object: the Python side threads a `SessionState` through
//! `execute()` calls, but the only field steps 1–3 read or write is the frame.

pub mod deduplicate;
pub mod normalize;
pub mod optical;

use crate::table::{Cell, DType};

/// `str(value)` on a cell, as `astype(str)` renders it.
///
/// A null renders `"nan"` — that is what pandas writes for both `np.nan` in a
/// float column and `np.nan` sitting in an object column, and Stage 4 depends
/// on it: an all-missing `imagetyp` becomes the literal `'NAN'` and every row
/// is dropped.
pub fn astype_str(cell: &Cell) -> String {
    match cell {
        Cell::Null => "nan".to_string(),
        Cell::Str(s) => s.clone(),
        Cell::Int(i) => i.to_string(),
        Cell::Bool(b) => (if *b { "True" } else { "False" }).to_string(),
        // Rust's shortest-round-trip Display matches Python's repr for every
        // value this pipeline can put in a string context; the pair disagree
        // only on exponent formatting (`1e+20` vs `100000000000000000000`),
        // which no header field reaches.
        Cell::Float(f) => format_python_float(*f),
    }
}

fn format_python_float(f: f64) -> String {
    if f.is_nan() {
        "nan".to_string()
    } else if f.is_infinite() {
        (if f > 0.0 { "inf" } else { "-inf" }).to_string()
    } else if f == f.trunc() && f.abs() < 1e16 {
        // Python always shows a decimal point on a whole float: 1.0, not 1.
        format!("{f:.1}")
    } else {
        format!("{f}")
    }
}

/// `pd.to_numeric(x, errors='coerce')` for one cell.
///
/// String parsing goes through the CSV tokenizer's converter, not Rust's:
/// `to_numeric` uses the same `precise_xstrtod` as `read_csv` (verified on
/// pandas 2.2.3 — both return `2.4559286` for `"2.4559285999999996"`, where a
/// correctly rounded parse returns the 17-digit value).
pub fn to_numeric(cell: &Cell) -> Option<f64> {
    match cell {
        Cell::Null => None,
        Cell::Int(i) => Some(*i as f64),
        Cell::Float(f) => {
            if f.is_nan() {
                None
            } else {
                Some(*f)
            }
        }
        Cell::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Cell::Str(s) => crate::table::parse_float(s),
    }
}

/// `float(x)` — the Python builtin, which *is* correctly rounded, unlike the
/// pandas converter above. Used only where the Python source calls `float()`
/// directly on a cell (the master-preference group key).
///
/// `Err` stands for the `ValueError`/`TypeError` those call sites catch. A
/// null is not an error: a missing cell is `np.nan`, and `float(nan)` is
/// `nan`.
pub fn python_float(cell: &Cell) -> Result<f64, ()> {
    match cell {
        Cell::Null => Ok(f64::NAN),
        Cell::Int(i) => Ok(*i as f64),
        Cell::Float(f) => Ok(*f),
        Cell::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Cell::Str(s) => s.trim().parse::<f64>().map_err(|_| ()),
    }
}

/// The dtype two columns take when one fills the other's gaps
/// (`Series.fillna(other)`) or when duplicate-named columns are coalesced.
///
/// Only the cases the corpus reaches are resolved. Anything mixing a string
/// with a number would become an `object` column in pandas, and rendering the
/// numbers into it means reproducing Python's `repr` — worth doing when a
/// fixture actually needs it, not guessing at now.
pub fn promote(a: DType, b: DType) -> Option<DType> {
    use DType::*;
    match (a, b) {
        (x, y) if x == y => Some(x),
        (Int, Float) | (Float, Int) => Some(Float),
        _ => None,
    }
}

/// Casts a cell into an already-promoted dtype.
pub fn cast(cell: &Cell, to: DType) -> Cell {
    match (cell, to) {
        (Cell::Int(i), DType::Float) => Cell::Float(*i as f64),
        _ => cell.clone(),
    }
}

//! The pipeline steps, in order, plus the pandas-semantics helpers they share.
//!
//! Each step is a pure function `Table -> Table` rather than a mutation of a
//! shared state object: the Python side threads a `SessionState` through
//! `execute()` calls, but the only field steps 1–3 read or write is the frame.

pub mod aggregate;
pub mod calibration;
pub mod deduplicate;
pub mod geocode;
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
/// The `str(e)` of the exception `float(cell)` raises, for the log records
/// that interpolate it. `python_float` fails on exactly one thing -- a string
/// that does not parse -- so this is CPython's `ValueError` message, not a
/// general emulation of it.
pub fn python_float_error(cell: &Cell) -> String {
    match cell {
        Cell::Str(s) => format!("could not convert string to float: '{s}'"),
        other => format!("float() argument must be a string or a real number, not '{other:?}'"),
    }
}

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

/// The mean pandas' `groupby(...).agg('mean')` computes: a **Kahan
/// compensated** running sum in row order, divided by the count.
///
/// This is not `Series.mean()` and not `np.mean()` — both of those use
/// pairwise summation and give a different last bit. Measured on pandas
/// 2.2.3 with `[1.0] + [1e-16] * 10`:
///
/// ```text
/// groupby.mean  0.09090909090909101   (Kahan)
/// Series.mean   0.09090909090909097   (pairwise)
/// naive sum/n   0.09090909090909091
/// ```
///
/// Every `'mean'` in the six pipeline steps goes through a groupby, so this
/// is the one they need. `reports.py`'s "Mean temperature" line is the single
/// bare `Series.mean()` in the codebase; it uses [`pairwise_mean`] instead.
pub fn kahan_mean(values: impl IntoIterator<Item = f64>) -> f64 {
    let mut n = 0usize;
    let sum = kahan_sum(values.into_iter().inspect(|_| n += 1));
    sum / n as f64
}

/// `groupby(...).agg('sum')`, which is compensated the same way. Measured on
/// the same input as above: pandas gives `1.000000000000001` where a naive
/// left fold gives `1.0`.
pub fn kahan_sum(values: impl IntoIterator<Item = f64>) -> f64 {
    let mut sum = 0.0f64;
    let mut compensation = 0.0f64;
    for v in values {
        let y = v - compensation;
        let t = sum + y;
        compensation = (t - sum) - y;
        sum = t;
    }
    sum
}

/// numpy's `add.reduce` on a contiguous float64 array: **pairwise**
/// summation, not the Kahan compensation `groupby` uses and not a left fold.
///
/// `Series.mean()` reaches this through `nanops.nanmean`, which fills NA with
/// zero, sums the *whole* array, and divides by the non-NA count -- so the
/// tree shape depends on the row count including the missing rows, and
/// dropping them first would change the answer.
///
/// Transcribed from numpy's `pairwise_sum_@TYPE@` (`loops.c.src`): a plain
/// loop below 8 elements, an 8-accumulator unrolled pass with a fixed
/// reduction tree up to `PW_BLOCKSIZE`, and a recursive split above it whose
/// halves are rounded down to a multiple of 8.
///
/// The corpus can see this, which is worth saying because most last-bit
/// hazards in this port cannot be. Measured on the "Mean temperature" line of
/// both fixtures (`parity/check_reports.py` compares the bits):
///
/// ```text
///                n    pandas             left fold          Kahan
/// sadr           15   ...402b40 (0x27)   ...402b40 (0x27)   ...402b40 (0x29)
/// sh2101_calib   28   ...802d40 (0xfa)   ...802d40 (0xf9)   ...802d40 (0xfa)
/// ```
///
/// Neither alternative matches both fixtures; pairwise matches both.
pub fn pairwise_sum(a: &[f64]) -> f64 {
    const PW_BLOCKSIZE: usize = 128;
    let n = a.len();
    if n < 8 {
        let mut res = 0.0f64;
        for &v in a {
            res += v;
        }
        res
    } else if n <= PW_BLOCKSIZE {
        let mut r = [
            a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7],
        ];
        let mut i = 8;
        while i < n - (n % 8) {
            for (j, acc) in r.iter_mut().enumerate() {
                *acc += a[i + j];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        while i < n {
            res += a[i];
            i += 1;
        }
        res
    } else {
        let mut n2 = n / 2;
        n2 -= n2 % 8;
        pairwise_sum(&a[..n2]) + pairwise_sum(&a[n2..])
    }
}

/// `Series.mean()` — [`pairwise_sum`] over the NA-as-zero values, divided by
/// the non-NA count. An all-missing series divides 0.0 by 0.0 and yields NaN,
/// as pandas does.
pub fn pairwise_mean(values: &[f64], count: usize) -> f64 {
    pairwise_sum(values) / count as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kahan_sum_matches_pandas_groupby_sum() {
        let mut v = vec![1.0f64];
        v.extend(std::iter::repeat(1e-16).take(10));
        assert_eq!(kahan_sum(v.iter().copied()), 1.000000000000001);
        assert_eq!(v.iter().sum::<f64>(), 1.0); // the naive fold loses them
    }

    /// numpy's pairwise tree is not a left fold, and the difference is
    /// visible in the last bit at n >= 8.
    #[test]
    fn pairwise_sum_is_not_a_left_fold() {
        let mut v = vec![1.0f64];
        v.extend(std::iter::repeat(1e-16).take(10));
        let naive = v.iter().fold(0.0f64, |a, b| a + b);
        assert_eq!(naive, 1.0);
        assert_ne!(pairwise_sum(&v), naive);
        // Below the 8-element threshold the two are the same algorithm.
        assert_eq!(pairwise_sum(&v[..7]), v[..7].iter().fold(0.0, |a, b| a + b));
    }

    /// Bit patterns from `numpy.array(v).sum()` on numpy 2.x, for lengths
    /// covering all three branches: the unrolled block (15, 28) and the
    /// recursive split above `PW_BLOCKSIZE` (200, 300). A left fold and a
    /// Kahan sum each give a *different* answer for every one of these, so
    /// this pins the tree shape rather than merely the arithmetic.
    #[test]
    fn pairwise_sum_matches_numpy_across_all_three_branches() {
        for (n, want) in [
            (15usize, 0x3ff0000000000003u64),
            (28, 0x3ff0000000000009),
            (200, 0x3ff0000000000055),
            (300, 0x3ff0000000000082),
        ] {
            let mut v = vec![1.0f64];
            v.extend(std::iter::repeat(1e-16).take(n - 1));
            assert_eq!(pairwise_sum(&v).to_bits(), want, "n = {n}");
            assert_ne!(v.iter().fold(0.0f64, |a, b| a + b).to_bits(), want, "n = {n}");
            assert_ne!(kahan_sum(v.iter().copied()).to_bits(), want, "n = {n}");
        }
    }

    #[test]
    fn kahan_mean_matches_pandas_groupby_not_numpy() {
        let mut v = vec![1.0f64];
        v.extend(std::iter::repeat(1e-16).take(10));
        assert_eq!(kahan_mean(v.iter().copied()), 0.09090909090909101);
        // The naive sum loses every small term.
        let naive: f64 = v.iter().sum::<f64>() / v.len() as f64;
        assert_ne!(naive, 0.09090909090909101);
    }
}

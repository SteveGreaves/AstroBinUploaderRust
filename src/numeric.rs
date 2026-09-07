//! The two rounding algorithms the Python side uses, and the parsing helpers
//! that feed them.
//!
//! Both are live and they disagree (PORT_PLAN.md hazard 3). Picking the wrong
//! one changes printed output at exact decimal boundaries:
//!
//! | x     | `python_round` | `numpy_round` |
//! |-------|----------------|---------------|
//! | 2.675 | 2.67           | 2.68          |
//! | 2.665 | 2.67           | 2.66          |
//! | 1.115 | 1.11           | 1.12          |
//! | 0.005 | 0.01           | 0.00          |
//! | 3.345 | 3.35           | 3.34          |
//!
//! Call sites:
//! - `optical.py` HFR / IMSCALE / FWHM  -> [`python_round`]
//! - `base.py` Stage 7 `exposure`, `gain` -> [`numpy_round`]

/// CPython's builtin `round(x, n)`: correctly rounded on the true decimal
/// value, ties to even.
///
/// Implemented via the shortest round-tripping decimal representation rather
/// than arithmetic, because that is where builtin `round`'s correctness comes
/// from -- `x * 10^n` in binary already carries the error being avoided.
pub fn python_round(x: f64, n: u32) -> f64 {
    if !x.is_finite() {
        return x;
    }
    // Format to n decimals using Rust's formatter, which is itself correctly
    // rounded ties-to-even on the exact binary value -- the same rule CPython
    // applies -- then read back.
    let s = format!("{:.*}", n as usize, x);
    s.parse::<f64>().unwrap_or(x)
}

/// numpy / pandas `.round(n)`: multiply, `rint` (ties to even on the binary
/// double), divide. Reproduced arithmetically because the error it introduces
/// *is* the behaviour being matched.
pub fn numpy_round(x: f64, n: u32) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let f = 10f64.powi(n as i32);
    let scaled = x * f;
    // f64::round_ties_even matches numpy's rint under the default rounding mode.
    scaled.round_ties_even() / f
}

/// `pd.to_numeric(errors='coerce')` for a single value: anything unparseable
/// becomes null rather than raising.
pub fn to_numeric(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    t.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The five boundary values measured in REMEDIATION_PLAN.md A12.
    #[test]
    fn the_two_algorithms_disagree_exactly_where_python_says_they_do() {
        let cases = [
            (2.675, 2.67, 2.68),
            (2.665, 2.67, 2.66),
            (1.115, 1.11, 1.12),
            (0.005, 0.01, 0.00),
            (3.345, 3.35, 3.34),
        ];
        for (x, py, np) in cases {
            assert_eq!(format!("{:.2}", python_round(x, 2)), format!("{py:.2}"), "python_round({x})");
            assert_eq!(format!("{:.2}", numpy_round(x, 2)), format!("{np:.2}"), "numpy_round({x})");
        }
    }

    #[test]
    fn rounding_to_zero_places_matches_pandas_gain_hardening() {
        // base.py Stage 7: gain = to_numeric(...).fillna(0).round().astype(int)
        assert_eq!(numpy_round(100.4, 0) as i64, 100);
        assert_eq!(numpy_round(100.5, 0) as i64, 100); // ties to even
        assert_eq!(numpy_round(101.5, 0) as i64, 102);
        assert_eq!(numpy_round(-0.5, 0) as i64, 0);
    }

    #[test]
    fn non_finite_passes_through() {
        assert!(python_round(f64::NAN, 2).is_nan());
        assert!(numpy_round(f64::NAN, 2).is_nan());
        assert_eq!(python_round(f64::INFINITY, 2), f64::INFINITY);
    }

    #[test]
    fn to_numeric_coerces_rather_than_failing() {
        assert_eq!(to_numeric("100"), Some(100.0));
        assert_eq!(to_numeric(" 1.5 "), Some(1.5));
        assert_eq!(to_numeric("abc"), None);
        assert_eq!(to_numeric(""), None);
    }
}

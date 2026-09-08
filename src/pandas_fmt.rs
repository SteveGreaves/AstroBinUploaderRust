//! `DataFrame.to_string(index=False)`, transcribed.
//!
//! PORT_PLAN.md hazard 1: the bottom of every session summary is a pandas text
//! render of the acquisition table, and its layout rules are not the obvious
//! ones. Measured against pandas 2.2.3 rather than read off the docs:
//!
//! ```text
//! '  a  bbbbbb    s'
//! '  1    1.50    x'
//! '222    2.25 yyyy'
//! ```
//!
//! Two spaces appear between the numeric columns and one before the object
//! column. The extra space is **not** injected into the values — with
//! `index=False` pandas passes `leading_space=False` down to every array
//! formatter, so no value gets a prefix. It comes from the *header*:
//! `DataFrameFormatter._get_formatted_column_labels` does
//!
//! ```python
//! [" " + x if not self._get_formatter(i) and need_leadsp[x] else x]
//! ```
//!
//! where `need_leadsp` is `is_numeric_dtype` per column. So a numeric column's
//! header is one character wider than its name, and since the column width is
//! `max(len(header), max(len(value)))` with everything right-justified and the
//! columns adjoined by a single space, a numeric column whose values are
//! narrower than its name gains a leading blank.
//!
//! Floats get a per-column common decimal count, and that too is not what it
//! looks like. Every value is first rendered `%.6f` (`display.precision`),
//! then `_trim_zeros_float` strips one trailing character at a time from
//! *every* value for as long as they all still end in `0` — so a single
//! 3-decimal value in the column forces `4.6` to print as `4.600`.

use crate::table::{Cell, DType, Table};

/// `display.precision`.
const DIGITS: usize = 6;

/// `to_string(index=False, max_rows=None, max_cols=None)` with the default
/// `justify='right'` and `na_rep='NaN'`.
pub fn to_string_index_false(t: &Table) -> String {
    // `DataFrameRenderer` short-circuits an empty frame before any column
    // formatting runs. Reachable only from a session with no LIGHT frames at
    // all, which also produces an empty report body.
    if t.n_rows == 0 || t.columns.is_empty() {
        let names: Vec<String> = t.columns.iter().map(|c| c.name.clone()).collect();
        return format!(
            "Empty DataFrame\nColumns: [{}]\nIndex: []",
            names.join(", ")
        );
    }

    let mut cols: Vec<Vec<String>> = Vec::with_capacity(t.columns.len());
    for col in &t.columns {
        let values = format_column(col);
        // `" " + name` for a numeric dtype; bool counts as numeric in pandas.
        let header = if is_numeric(col.dtype) {
            format!(" {}", col.name)
        } else {
            col.name.clone()
        };
        let width = values
            .iter()
            .map(|v| v.chars().count())
            .chain(std::iter::once(header.chars().count()))
            .max()
            .unwrap_or(0);
        let mut cell_col = Vec::with_capacity(t.n_rows + 1);
        cell_col.push(rjust(&header, width));
        for v in &values {
            cell_col.push(rjust(v, width));
        }
        cols.push(cell_col);
    }

    // `adjoin(1, *strcols)`.
    (0..=t.n_rows)
        .map(|r| {
            cols.iter()
                .map(|c| c[r].as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_numeric(d: DType) -> bool {
    matches!(d, DType::Int | DType::Float | DType::Bool)
}

fn rjust(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n >= width {
        s.to_string()
    } else {
        format!("{}{}", " ".repeat(width - n), s)
    }
}

/// `format_array(values, None, leading_space=False, na_rep="NaN")`.
fn format_column(col: &crate::table::Column) -> Vec<String> {
    match col.dtype {
        DType::Float => format_floats(&col.cells),
        DType::Int => col
            .cells
            .iter()
            .map(|c| match c {
                Cell::Int(v) => v.to_string(),
                // An int64 column has no null slot in numpy; a null here
                // would mean the dtype tag lied.
                other => generic_str(other),
            })
            .collect(),
        // `_GenericArrayFormatter._format` — `str(v)`, with the NA sentinel
        // rendered as `na_rep`. A literal Python `None` in an object column
        // renders `None` instead, but every null this pipeline produces is
        // `np.nan` (read_csv's NA sentinels and every step's fill), so `NaN`
        // is the reachable spelling.
        DType::Bool | DType::Str | DType::DateTime => {
            col.cells.iter().map(generic_str).collect()
        }
    }
}

fn generic_str(c: &Cell) -> String {
    match c {
        Cell::Null => "NaN".to_string(),
        Cell::Str(s) => s.clone(),
        Cell::Int(i) => i.to_string(),
        Cell::Bool(b) => (if *b { "True" } else { "False" }).to_string(),
        Cell::Float(f) => crate::steps::astype_str(&Cell::Float(*f)),
    }
}

/// `FloatArrayFormatter.get_result_as_array` for the default (no explicit
/// `float_format`) case.
fn format_floats(cells: &[Cell]) -> Vec<String> {
    let values: Vec<Option<f64>> = cells
        .iter()
        .map(|c| match c {
            Cell::Float(v) if !v.is_nan() => Some(*v),
            Cell::Int(i) => Some(*i as f64),
            _ => None,
        })
        .collect();

    let fixed = trim_zeros_float(
        values
            .iter()
            .map(|v| match v {
                Some(x) => format!("{:.*}", DIGITS, x),
                None => "NaN".to_string(),
            })
            .collect(),
    );

    // The scientific-notation fallback. No fixture reaches it — every float in
    // the acquisition frame has already been rounded to two decimals — but it
    // is five lines and a user's data can hit it, so it is transcribed rather
    // than left as a hole. `10 ** -digits` is the small-value threshold and
    // `1e6` the large one; both are pandas' own arbitrary constants.
    let maxlen = fixed.iter().map(|s| s.chars().count()).max().unwrap_or(0);
    let too_long = maxlen > DIGITS + 6;
    let has_large = values.iter().any(|v| v.is_some_and(|x| x.abs() > 1e6));
    let has_small = values
        .iter()
        .any(|v| v.is_some_and(|x| x.abs() < 1e-6 && x.abs() > 0.0));
    if has_small || (too_long && has_large) {
        return trim_zeros_float(
            values
                .iter()
                .map(|v| match v {
                    Some(x) => python_exponential(*x),
                    None => "NaN".to_string(),
                })
                .collect(),
        );
    }
    fixed
}

/// `f"{x:.6e}"`. Rust's `{:e}` writes the exponent bare (`e-9`); Python's
/// always carries a sign and at least two digits (`e-09`).
fn python_exponential(x: f64) -> String {
    let s = format!("{:.*e}", DIGITS, x);
    let Some((mantissa, exp)) = s.split_once('e') else {
        return s; // inf / NaN never reach here, but don't corrupt them if they do
    };
    let (sign, digits) = match exp.strip_prefix('-') {
        Some(rest) => ('-', rest),
        None => ('+', exp),
    };
    format!("{mantissa}e{sign}{:0>2}", digits)
}

/// `pandas.io.formats.format._trim_zeros_float`.
///
/// Strips one trailing character from every decimal-looking entry for as long
/// as they *all* end in `0`, then restores a single `0` after a bare decimal
/// point. Entries that are not decimal-looking (`NaN`, `inf`, scientific
/// notation) are left alone and take no part in the test — but they still
/// count toward the column width.
fn trim_zeros_float(mut values: Vec<String>) -> Vec<String> {
    fn is_number_with_decimal(s: &str) -> bool {
        // ^\s*[\+-]?[0-9]+\.[0-9]*$
        let t = s.trim_start();
        let t = t.strip_prefix(['+', '-']).unwrap_or(t);
        let Some((int_part, frac)) = t.split_once('.') else {
            return false;
        };
        !int_part.is_empty()
            && int_part.bytes().all(|b| b.is_ascii_digit())
            && frac.bytes().all(|b| b.is_ascii_digit())
    }

    loop {
        let numbers: Vec<&String> = values
            .iter()
            .filter(|v| is_number_with_decimal(v))
            .collect();
        if numbers.is_empty() || !numbers.iter().all(|v| v.ends_with('0')) {
            break;
        }
        values = values
            .into_iter()
            .map(|v| {
                if is_number_with_decimal(&v) {
                    let mut v = v;
                    v.pop();
                    v
                } else {
                    v
                }
            })
            .collect();
    }

    values
        .into_iter()
        .map(|v| {
            if v.ends_with('.') && is_number_with_decimal(&v) {
                format!("{v}0")
            } else {
                v
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::Column;

    fn col(name: &str, dtype: DType, cells: Vec<Cell>) -> Column {
        Column {
            name: name.into(),
            dtype,
            cells,
        }
    }

    fn table(columns: Vec<Column>) -> Table {
        let n_rows = columns[0].cells.len();
        Table { columns, n_rows }
    }

    /// The frame from hazard 1, measured on pandas 2.2.3.
    #[test]
    fn the_separator_is_not_uniform() {
        let t = table(vec![
            col("a", DType::Int, vec![Cell::Int(1), Cell::Int(222)]),
            col(
                "bbbbbb",
                DType::Float,
                vec![Cell::Float(1.5), Cell::Float(2.25)],
            ),
            col(
                "s",
                DType::Str,
                vec![Cell::Str("x".into()), Cell::Str("yyyy".into())],
            ),
        ]);
        assert_eq!(
            to_string_index_false(&t),
            "  a  bbbbbb    s\n  1    1.50    x\n222    2.25 yyyy"
        );
    }

    /// A single 3-decimal value changes how every other value in the column
    /// prints.
    #[test]
    fn floats_share_one_decimal_count_per_column() {
        let t = table(vec![col(
            "v",
            DType::Float,
            vec![Cell::Float(4.6), Cell::Float(4.75), Cell::Float(100.125)],
        )]);
        assert_eq!(
            to_string_index_false(&t),
            "      v\n  4.600\n  4.750\n100.125"
        );
    }

    #[test]
    fn a_numeric_header_is_one_wider_than_its_name_and_an_object_header_is_not() {
        let t = table(vec![col("vvvvvv", DType::Float, vec![Cell::Float(1.5)])]);
        assert_eq!(to_string_index_false(&t), " vvvvvv\n    1.5");
        let t = table(vec![col("vvvvvv", DType::Int, vec![Cell::Int(15)])]);
        assert_eq!(to_string_index_false(&t), " vvvvvv\n     15");
        let t = table(vec![col(
            "vvvvvv",
            DType::Str,
            vec![Cell::Str("ab".into())],
        )]);
        assert_eq!(to_string_index_false(&t), "vvvvvv\n    ab");
    }

    /// A bool column is `is_numeric_dtype` in pandas, so it gets the header's
    /// leading space — but its *values* go through the generic formatter, not
    /// the integer one, and render as `True`/`False`. Measured on pandas
    /// 2.2.3; reachable through `[defaults]`, since `table.rs` infers
    /// `True`/`False` as bool.
    #[test]
    fn a_bool_column_takes_the_numeric_header_space_and_the_generic_values() {
        let t = table(vec![
            col("b", DType::Bool, vec![Cell::Bool(true), Cell::Bool(false)]),
            col("x", DType::Float, vec![Cell::Float(1.5), Cell::Float(2.0)]),
            col(
                "s",
                DType::Str,
                vec![Cell::Str("a".into()), Cell::Str("b".into())],
            ),
        ]);
        assert_eq!(
            to_string_index_false(&t),
            "    b   x s\n True 1.5 a\nFalse 2.0 b"
        );
        let t = table(vec![col("bb", DType::Bool, vec![Cell::Bool(true)])]);
        assert_eq!(to_string_index_false(&t), "  bb\nTrue");
    }

    #[test]
    fn whole_floats_keep_exactly_one_decimal() {
        let t = table(vec![col(
            "v",
            DType::Float,
            vec![Cell::Float(600.0), Cell::Float(0.0)],
        )]);
        assert_eq!(to_string_index_false(&t), "    v\n600.0\n  0.0");
    }

    #[test]
    fn nan_renders_as_na_rep_and_blocks_no_trimming() {
        let t = table(vec![col(
            "v",
            DType::Float,
            vec![Cell::Null, Cell::Float(5.0)],
        )]);
        assert_eq!(to_string_index_false(&t), "  v\nNaN\n5.0");
    }

    /// `has_small_values` — any non-zero magnitude below 1e-6 switches the
    /// whole column to scientific notation, and that format string carries a
    /// hardcoded leading space.
    #[test]
    fn a_tiny_value_switches_the_column_to_scientific_notation() {
        let t = table(vec![col(
            "v",
            DType::Float,
            vec![Cell::Float(1e-9), Cell::Float(2.0)],
        )]);
        assert_eq!(
            to_string_index_false(&t),
            "           v\n1.000000e-09\n2.000000e+00"
        );
    }

    #[test]
    fn an_empty_frame_short_circuits_before_any_column_formatting() {
        let t = Table {
            columns: vec![
                col("date", DType::Str, vec![]),
                col("n", DType::Int, vec![]),
            ],
            n_rows: 0,
        };
        assert_eq!(
            to_string_index_false(&t),
            "Empty DataFrame\nColumns: [date, n]\nIndex: []"
        );
    }
}

//! The Rust half of the per-step parity oracle.
//!
//! Emits exactly the byte sequence `parity/dump_steps.py` emits for the same
//! frame, so the two can be diffed directly:
//!
//! ```text
//! STEP<TAB>step_name<TAB>column<TAB>dtype<TAB>v1|v2|v3...
//! ```
//!
//! Every detail below is a contract with that script, not a choice:
//!
//! - Columns are emitted in **frame order, never sorted**. `dump_steps.py`
//!   iterates `df.columns`. Sorting here would hide column-ordering bugs, and
//!   column order is load-bearing: `NormalizeHeadersStep` stage 2 sorts the
//!   columns *only* when duplicate names had to be coalesced.
//! - Floats are the raw IEEE-754 bits, little-endian, lower-case hex, prefixed
//!   `f` — `struct.pack("<d", v).hex()`. Exact and free of the repr
//!   disagreements between Python and Rust.
//! - Null is a bare `0x00` byte with no type prefix.
//! - The escape order in string cells is mandatory: backslash first, then the
//!   `|` separator, then newline. Any other order corrupts the output.
//! - The datetime tag is the bare string `datetime64`, not pandas' full
//!   `datetime64[ns]`; `dump_steps.py:72` hardcodes it while every other
//!   column uses `str(series.dtype)`. Match the inconsistency.
//!
//! Object columns are rendered as plain strings. `canon()` would fall through
//! to Python's `str(value)` for a float sitting inside an object column, which
//! would reintroduce repr disagreement — but no such cell exists in either
//! fixture at any step (checked: the only numeric-looking object cells are
//! `rotantang`/`rotatang` = `"0"` from `[defaults]` and `filter_code` =
//! `"4663"` from `[filters]`, both genuinely strings). If one ever appears,
//! this is where it has to be handled.

use crate::table::{Cell, DType, Table};
use std::io::Write;

pub const NULL: &str = "\u{0}";
pub const SEP: char = '|';

/// The pandas dtype string for a column.
pub fn dtype_tag(d: DType) -> &'static str {
    match d {
        DType::Int => "int64",
        DType::Float => "float64",
        DType::Bool => "bool",
        DType::Str => "object",
        DType::DateTime => "datetime64",
    }
}

/// One cell in canonical form.
pub fn canon(cell: &Cell) -> String {
    match cell {
        Cell::Null => NULL.to_string(),
        Cell::Float(v) => format!("f{}", hex_bits(*v)),
        Cell::Int(v) => format!("i{v}"),
        Cell::Bool(b) => (if *b { "b1" } else { "b0" }).to_string(),
        Cell::Str(s) => format!("s{}", escape(s)),
    }
}

/// `struct.pack("<d", v).hex()` — little-endian, lower-case.
fn hex_bits(v: f64) -> String {
    let mut out = String::with_capacity(16);
    for b in v.to_bits().to_le_bytes() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Backslash, then separator, then newline. The order is not negotiable.
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(SEP, "\\p")
        .replace('\n', "\\n")
}

/// Writes one frame in the canonical form, columns in frame order.
pub fn dump_frame(step_name: &str, table: &Table, out: &mut impl Write) -> std::io::Result<()> {
    // The column list first, on one line. Column order is load-bearing, and on
    // the disk-scan path it is decided by first appearance across every file
    // scanned -- so one unexpected card in the first file shifts every later
    // column, and a per-column diff would show sixty shifted lines with no
    // obvious cause. This makes that failure a single short diff.
    let names: Vec<&str> = table.columns.iter().map(|c| c.name.as_str()).collect();
    writeln!(out, "COLS\t{step_name}\t{}", names.join(&SEP.to_string()))?;

    for col in &table.columns {
        let rendered: Vec<String> = col.cells.iter().map(canon).collect();
        writeln!(
            out,
            "STEP\t{step_name}\t{}\t{}\t{}",
            col.name,
            dtype_tag(col.dtype),
            rendered.join(&SEP.to_string())
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_bits_are_little_endian_lowercase_hex() {
        // struct.pack("<d", 600.0).hex() == '0000000000c08240'
        assert_eq!(hex_bits(600.0), "0000000000c08240");
        // struct.pack("<d", 1.0).hex() == '000000000000f03f'
        assert_eq!(hex_bits(1.0), "000000000000f03f");
    }

    #[test]
    fn escape_handles_backslash_before_separator() {
        // A literal backslash must not turn a following 'p' into an escape.
        assert_eq!(escape("a\\b|c"), "a\\\\b\\pc");
        assert_eq!(escape("line\nnext"), "line\\nnext");
    }

    #[test]
    fn null_is_a_bare_nul_byte() {
        assert_eq!(canon(&Cell::Null), "\u{0}");
    }

    #[test]
    fn scalars_carry_their_type_prefix() {
        assert_eq!(canon(&Cell::Int(16)), "i16");
        assert_eq!(canon(&Cell::Bool(true)), "b1");
        assert_eq!(canon(&Cell::Bool(false)), "b0");
        assert_eq!(canon(&Cell::Str("LIGHT".into())), "sLIGHT");
    }

    #[test]
    fn datetime_tag_matches_the_scripts_hardcoded_spelling() {
        assert_eq!(dtype_tag(DType::DateTime), "datetime64");
    }
}

//! CSV ingest with pandas-equivalent dtype inference.
//!
//! `HeaderExtractor.extract_from_csv` is three lines of Python —
//! `pd.read_csv(path)` then upper-case the column names — but those three
//! lines decide the dtype of every column, and dtype is visible in the
//! acquisition CSV that AstroBin consumes: an `int64` gain renders `100`,
//! a `float64` gain renders `100.0`.
//!
//! Rules below were measured against pandas 2.2.3 with default arguments
//! (see RUST_PORT_PLAN.md hazard 14), not taken from documentation:
//!
//! | Input column                    | pandas dtype |
//! |---------------------------------|--------------|
//! | `100`, `100`                    | `int64`      |
//! | `100`, ``, `100`                | `float64`    |
//! | `100`, `None`, `100`            | `float64`    |
//! | `None`, `None`                  | `float64` (all NaN) |
//! | `abc`, `None`                   | `object` (`None` → NaN) |
//! | `100`, `1.5`                    | `float64`    |
//! | `True`, `False`                 | `bool`       |
//! | `007`, `008`                    | `int64` (→ 7, 8) |
//! | `99999999999999999999`          | `object` (i64 overflow) |
//!
//! The two that catch people: `None` is an **NA sentinel**, not the string
//! `"None"` — which matters because `[defaults]` writes a literal `None` for
//! `INSTRUME`/`TELESCOP`/`FOCNAME`/`FWHEEL`/`ROTNAME` — and a single missing
//! field anywhere demotes an integer column to float for every row.

use anyhow::{bail, Context, Result};
use std::path::Path;

/// pandas' default `na_values`, verified from `pandas._libs.parsers.STR_NA_VALUES`.
/// Matching is exact and case-sensitive (`nan` and `NaN` are listed separately;
/// `None` is listed but `NONE` is not).
const NA_VALUES: &[&str] = &[
    "", "#N/A", "#N/A N/A", "#NA", "-1.#IND", "-1.#QNAN", "-NaN", "-nan", "1.#IND", "1.#QNAN",
    "<NA>", "N/A", "NA", "NULL", "NaN", "None", "n/a", "nan", "null",
];

fn is_na(s: &str) -> bool {
    NA_VALUES.contains(&s)
}

/// A column's inferred type, mirroring the pandas dtype it would carry.
///
/// `DateTime` is never produced by inference — `read_csv` leaves a date column
/// as `object`, exactly as pandas does without `parse_dates`. It exists because
/// later pipeline steps build real `datetime64` columns, and those must render
/// with their own dtype tag. Its cells are `Cell::Str`, holding the same text
/// `str(Timestamp)` produces on the Python side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DType {
    Int,
    Float,
    Bool,
    Str,
    /// Not produced by inference or by any step in the pipeline today; it
    /// exists because `dump_steps.py` tags such a column `datetime64` and the
    /// dump contract has to be able to say so.
    #[allow(dead_code)]
    DateTime,
}

/// One cell. `Null` is pandas' NaN / NA.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Null,
}

impl Cell {
    pub fn is_null(&self) -> bool {
        matches!(self, Cell::Null)
    }
}

#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub dtype: DType,
    pub cells: Vec<Cell>,
}

/// A parsed CSV: named columns, all of equal length.
#[derive(Debug, Clone, Default)]
pub struct Table {
    pub columns: Vec<Column>,
    pub n_rows: usize,
}

impl Column {
    /// A column of one repeated value, the way `df[name] = scalar` broadcasts.
    pub fn broadcast(name: &str, value: Cell, n_rows: usize) -> Self {
        let dtype = match value {
            Cell::Int(_) => DType::Int,
            Cell::Float(_) => DType::Float,
            Cell::Bool(_) => DType::Bool,
            // A broadcast NaN makes a float64 column, as in pandas.
            Cell::Str(_) | Cell::Null => {
                if matches!(value, Cell::Null) {
                    DType::Float
                } else {
                    DType::Str
                }
            }
        };
        Column {
            name: name.to_string(),
            dtype,
            cells: vec![value; n_rows],
        }
    }
}

impl Table {
    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }

    pub fn column_mut(&mut self, name: &str) -> Option<&mut Column> {
        self.columns.iter_mut().find(|c| c.name == name)
    }

    pub fn has_column(&self, name: &str) -> bool {
        self.columns.iter().any(|c| c.name == name)
    }

    /// The first column whose name matches case-insensitively, as Stage 1's
    /// `[c for c in df.columns if c.upper() == hw_key.upper()][0]` picks it.
    pub fn column_ignore_case(&self, name: &str) -> Option<&Column> {
        self.columns
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// `df[name] = column`: replaces in place, keeping the column's position,
    /// or appends at the end when the name is new. Position matters — the
    /// per-step dump compares column order.
    pub fn set_column(&mut self, name: &str, mut col: Column) {
        col.name = name.to_string();
        match self.columns.iter().position(|c| c.name == name) {
            Some(i) => self.columns[i] = col,
            None => self.columns.push(col),
        }
    }

    /// `df[name] = scalar`.
    pub fn set_scalar(&mut self, name: &str, value: Cell) {
        let col = Column::broadcast(name, value, self.n_rows);
        self.set_column(name, col);
    }

    pub fn drop_column(&mut self, name: &str) {
        self.columns.retain(|c| c.name != name);
    }

    /// Row selection *and* reordering in one: the frame containing `idx`'s
    /// rows, in `idx`'s order. Both `df[mask]` and the `pd.concat` in master
    /// preference reduce to this, which keeps every row operation a
    /// permutation of the same columns rather than a frame merge.
    pub fn take_rows(&self, idx: &[usize]) -> Table {
        Table {
            columns: self
                .columns
                .iter()
                .map(|c| Column {
                    name: c.name.clone(),
                    dtype: c.dtype,
                    cells: idx.iter().map(|&i| c.cells[i].clone()).collect(),
                })
                .collect(),
            n_rows: idx.len(),
        }
    }

    /// Reads a CSV the way `extract_from_csv` does: infer dtypes, then
    /// upper-case every column name.
    pub fn read_csv_upper(path: &Path) -> Result<Self> {
        let mut t = Self::read_csv(path)?;
        for c in &mut t.columns {
            c.name = c.name.to_uppercase();
        }
        Ok(t)
    }

    pub fn read_csv(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading CSV {}", path.display()))?;
        Self::parse_str(&text)
    }

    pub fn parse_str(text: &str) -> Result<Self> {
        let mut records = parse_records(text);
        if records.is_empty() {
            return Ok(Table::default());
        }
        let header = records.remove(0);
        let width = header.len();

        // Transpose into raw per-column string vectors.
        let mut raw: Vec<Vec<String>> = vec![Vec::with_capacity(records.len()); width];
        for (i, rec) in records.iter().enumerate() {
            if rec.len() != width {
                bail!(
                    "row {} has {} fields, expected {width}",
                    i + 2,
                    rec.len()
                );
            }
            for (col, field) in rec.iter().enumerate() {
                raw[col].push(field.clone());
            }
        }

        let n_rows = records.len();
        let columns = header
            .into_iter()
            .zip(raw)
            .map(|(name, values)| {
                let dtype = infer_dtype(&values);
                let cells = values.iter().map(|v| coerce(v, dtype)).collect();
                Column { name, dtype, cells }
            })
            .collect();

        Ok(Table { columns, n_rows })
    }
}

/// pandas' inference order: integer (only when nothing is missing), then
/// float, then bool, else string.
fn infer_dtype(values: &[String]) -> DType {
    let has_na = values.iter().any(|v| is_na(v));
    let non_na: Vec<&String> = values.iter().filter(|v| !is_na(v)).collect();

    if non_na.is_empty() {
        // An all-NA column is float64 (all NaN), never object.
        return DType::Float;
    }

    // Integers demote to float the moment any value is missing, which is how
    // gain 100 starts rendering as 100.0.
    if !has_na && non_na.iter().all(|v| parse_int(v).is_some()) {
        return DType::Int;
    }
    // An integer literal too large for int64 does NOT become a float in
    // pandas -- the column stays object. Check before the float branch,
    // since Rust would happily parse "99999999999999999999" as 1e20.
    if non_na
        .iter()
        .any(|v| is_int_shaped(v) && parse_int(v).is_none())
    {
        return DType::Str;
    }
    if non_na.iter().all(|v| parse_float(v).is_some()) {
        return DType::Float;
    }
    if non_na.iter().all(|v| parse_bool(v).is_some()) {
        return DType::Bool;
    }
    DType::Str
}

fn coerce(value: &str, dtype: DType) -> Cell {
    if is_na(value) {
        return Cell::Null;
    }
    match dtype {
        DType::Int => parse_int(value).map(Cell::Int).unwrap_or(Cell::Null),
        DType::Float => parse_float(value).map(Cell::Float).unwrap_or(Cell::Null),
        DType::Bool => parse_bool(value).map(Cell::Bool).unwrap_or(Cell::Null),
        DType::Str | DType::DateTime => Cell::Str(value.to_string()),
    }
}

/// True when the text is an integer literal (optional sign, then digits only),
/// regardless of whether it fits in an i64.
fn is_int_shaped(s: &str) -> bool {
    let t = s.trim();
    let body = t.strip_prefix(['+', '-']).unwrap_or(t);
    !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit())
}

/// Integer parse with pandas' semantics: surrounding whitespace tolerated,
/// leading zeros fine (`007` → 7), anything beyond i64 is *not* an integer
/// (pandas leaves such a column as object).
fn parse_int(s: &str) -> Option<i64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let body = t.strip_prefix(['+', '-']).unwrap_or(t);
    if body.is_empty() || !body.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse::<i64>().ok()
}

/// Float parse with pandas' semantics, which are **not** Rust's.
///
/// `read_csv`'s default `float_precision=None` selects `precise_xstrtod` in
/// `pandas/_libs/src/parser/tokenizer.c`, and that converter is not correctly
/// rounded: it accumulates at most 17 significant digits into a double, then
/// applies a single multiply or divide by a tabulated power of ten. Rust's
/// `str::parse::<f64>` *is* correctly rounded, so the two disagree by an ULP
/// on values whose text carries a full 17-digit repr — which is exactly what
/// the extractor writes into the fixtures. Measured on pandas 2.2.3:
///
/// ```text
/// "2.4559285999999996"  default/high/legacy -> 2.4559286           (107dd2e4bda50340)
///                       round_trip, strtod  -> 2.4559285999999996  (0f7dd2e4bda50340)
/// ```
///
/// 62 of the 221 `FWHM` values in `sadr_raw.csv` differ on this, so the
/// tokenizer's arithmetic has to be reproduced rather than improved on.
pub fn parse_float(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    precise_xstrtod(t).or_else(|| {
        // `precise_xstrtod` only accepts digit-shaped text. pandas recognises
        // the infinity spellings separately, in `_try_double_nogil`, before
        // giving up on a column; Rust's parser covers the same spellings and
        // no NA sentinel reaches here.
        match t {
            "inf" | "+inf" | "Inf" | "+Inf" | "Infinity" | "+Infinity" | "INF" => {
                Some(f64::INFINITY)
            }
            "-inf" | "-Inf" | "-Infinity" | "-INF" => Some(f64::NEG_INFINITY),
            _ => None,
        }
    })
}

/// pandas' `precise_xstrtod`, transcribed.
///
/// Consumes the whole field (leading and trailing ASCII whitespace aside) or
/// returns `None`; a partial parse is not a float to the tokenizer either.
fn precise_xstrtod(s: &str) -> Option<f64> {
    /// The converter stops accumulating after this many significant digits and
    /// tracks the rest in the exponent. This truncation is the source of the
    /// ULP difference above.
    const MAX_DIGITS: usize = 17;

    let b = s.as_bytes();
    let mut i = 0usize;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }

    let mut negative = false;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        negative = b[i] == b'-';
        i += 1;
    }

    let mut number = 0f64;
    let mut exponent: i32 = 0;
    let mut num_digits = 0usize;
    let mut num_decimals = 0usize;

    while i < b.len() && b[i].is_ascii_digit() {
        if num_digits < MAX_DIGITS {
            number = number * 10.0 + f64::from(b[i] - b'0');
            num_digits += 1;
        } else {
            // Past the cap the digit only shifts the decimal point.
            exponent += 1;
        }
        i += 1;
    }

    if i < b.len() && b[i] == b'.' {
        i += 1;
        while num_digits < MAX_DIGITS && i < b.len() && b[i].is_ascii_digit() {
            number = number * 10.0 + f64::from(b[i] - b'0');
            i += 1;
            num_digits += 1;
            num_decimals += 1;
        }
        if num_digits >= MAX_DIGITS {
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
        exponent -= num_decimals as i32;
    }

    if num_digits == 0 {
        return None;
    }
    if negative {
        number = -number;
    }

    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        let mut exp_negative = false;
        if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
            exp_negative = b[i] == b'-';
            i += 1;
        }
        let mut n: i32 = 0;
        let mut exp_digits = 0usize;
        while exp_digits < MAX_DIGITS && i < b.len() && b[i].is_ascii_digit() {
            n = n.saturating_mul(10).saturating_add(i32::from(b[i] - b'0'));
            exp_digits += 1;
            i += 1;
        }
        if exp_digits == 0 {
            return None; // ERROR_EXPONENT_EMPTY
        }
        if exp_negative {
            exponent -= n;
        } else {
            exponent += n;
        }
    }

    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    if i != b.len() {
        return None;
    }

    // One scaling operation, from the tabulated powers of ten.
    Some(if exponent > 308 {
        f64::INFINITY // ERANGE; pandas returns HUGE_VAL
    } else if exponent > 0 {
        number * pow10(exponent as usize)
    } else if exponent < -308 {
        if exponent < -616 {
            0.0
        } else {
            number / pow10((-308 - exponent) as usize) / pow10(308)
        }
    } else {
        number / pow10((-exponent) as usize)
    })
}

/// `10^n` as the C compiler would materialise the literal `1eN`: the correctly
/// rounded double for that decimal value, which is what tokenizer.c's static
/// table holds. `powi` is not a substitute — it is exact only up to 1e22.
fn pow10(n: usize) -> f64 {
    static TABLE: std::sync::OnceLock<Vec<f64>> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        (0..=308)
            .map(|i| format!("1e{i}").parse::<f64>().expect("power of ten"))
            .collect()
    });
    table.get(n).copied().unwrap_or(f64::INFINITY)
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim() {
        "True" | "TRUE" | "true" => Some(true),
        "False" | "FALSE" | "false" => Some(false),
        _ => None,
    }
}

/// Splits CSV text into records, honouring RFC4180 quoting and pandas'
/// `skip_blank_lines=True` (a wholly empty line is dropped, not read as a
/// row of missing values).
fn parse_records(text: &str) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut field = String::new();
    let mut record: Vec<String> = Vec::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => in_quotes = true,
            ',' => record.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                record.push(std::mem::take(&mut field));
                if !(record.len() == 1 && record[0].is_empty()) {
                    records.push(std::mem::take(&mut record));
                } else {
                    record.clear();
                }
            }
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        if !(record.len() == 1 && record[0].is_empty()) {
            records.push(record);
        }
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dtype_of(csv: &str) -> DType {
        Table::parse_str(csv).unwrap().column("g").unwrap().dtype
    }

    #[test]
    fn integer_column_stays_integer() {
        assert_eq!(dtype_of("g,h\n100,1\n100,2\n"), DType::Int);
    }

    #[test]
    fn one_missing_field_demotes_integers_to_float() {
        // This is the 100 -> 100.0 failure surface.
        assert_eq!(dtype_of("g,h\n100,1\n,2\n100,3\n"), DType::Float);
        assert_eq!(dtype_of("g,h\n100,1\nNone,2\n100,3\n"), DType::Float);
        assert_eq!(dtype_of("g,h\n100,1\nNA,2\n100,3\n"), DType::Float);
    }

    #[test]
    fn none_is_a_null_sentinel_not_the_string_none() {
        // [defaults] writes a literal None for INSTRUME/TELESCOP/FOCNAME/etc.
        assert_eq!(dtype_of("g,h\nNone,1\nNone,2\n"), DType::Float);
        let t = Table::parse_str("g,h\nabc,1\nNone,2\n").unwrap();
        let g = t.column("g").unwrap();
        assert_eq!(g.dtype, DType::Str);
        assert_eq!(g.cells[0], Cell::Str("abc".into()));
        assert_eq!(g.cells[1], Cell::Null);
    }

    #[test]
    fn mixed_int_and_float_is_float() {
        assert_eq!(dtype_of("g,h\n100,1\n1.5,2\n"), DType::Float);
    }

    #[test]
    fn booleans_and_leading_zeros_and_overflow() {
        assert_eq!(dtype_of("g,h\nTrue,1\nFalse,2\n"), DType::Bool);
        let t = Table::parse_str("g,h\n007,1\n008,2\n").unwrap();
        assert_eq!(t.column("g").unwrap().dtype, DType::Int);
        assert_eq!(t.column("g").unwrap().cells[0], Cell::Int(7));
        // Beyond i64, pandas keeps the column as object.
        assert_eq!(dtype_of("g,h\n99999999999999999999,1\n2,2\n"), DType::Str);
    }

    /// The pandas tokenizer's float conversion is not correctly rounded, and
    /// the fixtures contain values that expose it. Bit patterns taken from
    /// `struct.pack("<d", pd.read_csv(...)[col][row]).hex()` on pandas 2.2.3.
    #[test]
    fn floats_match_pandas_precise_xstrtod_not_correctly_rounded_strtod() {
        let cases = [
            ("2.4559285999999996", 0x107dd2e4bda50340u64),
            ("2.4702907555555553", 0x27c7b5cc27c30340),
            ("2.3123070444444442", 0x2d98f1d59a7f0240),
            ("2.3984799777777774", 0xb454454516300340),
        ];
        for (text, want_le_bits) in cases {
            let got = parse_float(text).unwrap();
            let got_hex: String = got
                .to_bits()
                .to_le_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            let want_hex: String = want_le_bits
                .to_be_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            assert_eq!(got_hex, want_hex, "{text}");
            // And it is genuinely different from a correctly rounded parse.
            assert_ne!(got.to_bits(), text.parse::<f64>().unwrap().to_bits(), "{text}");
        }
    }

    #[test]
    fn short_decimals_agree_with_the_correctly_rounded_parse() {
        for text in ["600.0", "1.5", "0.0", "-3.25", "1e3", "2.5E-4", "  7.5  "] {
            assert_eq!(
                parse_float(text).unwrap().to_bits(),
                text.trim().parse::<f64>().unwrap().to_bits(),
                "{text}"
            );
        }
    }

    #[test]
    fn non_numeric_text_is_not_a_float() {
        assert_eq!(parse_float("abc"), None);
        assert_eq!(parse_float("1.2.3"), None);
        assert_eq!(parse_float("1e"), None);
        assert_eq!(parse_float("--1"), None);
        assert!(parse_float("inf").unwrap().is_infinite());
    }

    #[test]
    fn blank_lines_are_skipped_not_read_as_missing_rows() {
        let t = Table::parse_str("g,h\n100,1\n\n100,2\n").unwrap();
        assert_eq!(t.n_rows, 2);
        assert_eq!(t.column("g").unwrap().dtype, DType::Int);
    }

    #[test]
    fn quoted_fields_may_contain_commas_and_quotes() {
        let t = Table::parse_str("a,b\n\"x,y\",\"he said \"\"hi\"\"\"\n").unwrap();
        assert_eq!(t.column("a").unwrap().cells[0], Cell::Str("x,y".into()));
        assert_eq!(
            t.column("b").unwrap().cells[0],
            Cell::Str("he said \"hi\"".into())
        );
    }

    #[test]
    fn column_names_are_upper_cased_like_extract_from_csv() {
        let mut t = Table::parse_str("imagetyp,Gain\nLIGHT,100\n").unwrap();
        for c in &mut t.columns {
            c.name = c.name.to_uppercase();
        }
        assert!(t.column("IMAGETYP").is_some());
        assert!(t.column("GAIN").is_some());
    }
}

//! `engine/extractor.py` — the disk-scan half of header ingestion.
//!
//! The `--test` CSV path (`Table::read_csv_upper`) and this one produce
//! frames that *look* alike and are built by different rules. Keeping the two
//! straight is most of the work here:
//!
//! | | `extract_from_csv` | `extract_from_directories` |
//! |---|---|---|
//! | dtypes | `read_csv` inference | `pd.DataFrame(list_of_dicts)` inference |
//! | `"None"` | an NA sentinel → NaN | the literal string `None` |
//! | column names | upper-cased | left exactly as the file spelled them |
//! | column order | the CSV header | first appearance across the scanned files |
//! | `SOURCE_PATH` | absent on pre-A2 captures | always present |
//!
//! The scan's column order is the one that bites: one unexpected card in the
//! first file shifts every column after it, which is why `dump.rs` emits the
//! column list on its own line.

use anyhow::{Context, Result};
use rayon::prelude::*;
use std::collections::HashMap;

use crate::constants as col;
use crate::pathutil;
use crate::table::{Cell, Column, DType, Table};

/// `file.lower().endswith(...)` in `extract_from_directories`.
///
/// Note what is *not* here: `.fz`. A tile-compressed capture named
/// `foo.fits.fz` — the conventional spelling — is silently skipped and never
/// reaches the reader at all. `parity/fixtures/binary/synthetic/` pins both
/// halves of that with two byte-identical files differing only in extension.
const SCAN_EXTENSIONS: [&str; 4] = [".fits", ".fit", ".fts", ".xisf"];

/// The FITS extensions `extract_single_file` dispatches to the FITS reader.
const FITS_EXTENSIONS: [&str; 3] = [".fits", ".fit", ".fts"];

/// One header value, before it becomes a cell.
///
/// `Commentary` exists because `HISTORY` and `COMMENT` repeat many times per
/// header and the master sub-exposure count is parsed out of them, so they
/// cannot be a last-write-wins scalar. astropy hands back a
/// `_HeaderCommentaryCards` whose `str()` is the lines joined by newlines, and
/// which — not being a `str` — escapes the quote-strip that every string value
/// goes through.
#[derive(Debug, Clone, PartialEq)]
pub enum HeaderValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Commentary(Vec<String>),
}

impl HeaderValue {
    /// `str(v).strip("'").strip('"')`, applied only to genuine strings —
    /// `isinstance(v, str)` is false for every other variant.
    fn strip_quotes(self) -> Self {
        match self {
            HeaderValue::Str(s) => {
                HeaderValue::Str(s.trim_matches('\'').trim_matches('"').to_string())
            }
            other => other,
        }
    }

    fn to_cell(&self) -> Cell {
        match self {
            HeaderValue::Int(i) => Cell::Int(*i),
            HeaderValue::Float(f) => Cell::Float(*f),
            HeaderValue::Bool(b) => Cell::Bool(*b),
            HeaderValue::Str(s) => Cell::Str(s.clone()),
            HeaderValue::Commentary(lines) => Cell::Str(lines.join("\n")),
        }
    }
}

/// One file's header: ordered key/value pairs, first-insertion order kept and
/// a later duplicate overwriting in place — Python `dict` semantics, which
/// decide the frame's column order.
#[derive(Debug, Clone, Default)]
pub struct Record {
    pub keys: Vec<String>,
    pub values: HashMap<String, HeaderValue>,
}

impl Record {
    pub fn insert(&mut self, key: &str, value: HeaderValue) {
        if self.values.insert(key.to_string(), value).is_none() {
            self.keys.push(key.to_string());
        }
    }

    pub fn get(&self, key: &str) -> Option<&HeaderValue> {
        self.values.get(key)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }
}

/// `extract_from_directories`: walk, filter, **sort**, read, assemble.
///
/// The Python side reads files with a `ProcessPoolExecutor` and reassembles
/// results in the sorted dispatch order rather than completion order —
/// because `as_completed()` is not that order, and every "first wins"
/// resolution downstream (dedup's survivor pick, master preference,
/// `agg('first')`, every `.iloc[0]` in `reports.py`) depends on row order
/// tracking the sort, not the read schedule.
///
/// `rayon`'s `par_iter().map(...).collect()` sidesteps that problem rather
/// than solving it the Python's way: a parallel map into an indexed
/// collection preserves input order by construction, so there is no
/// reassembly step to get wrong. Threads, not processes — there is no GIL to
/// escape from, and no result needs to cross a process boundary.
///
/// On this project's own hardware the win is not CPU time — a single header
/// read is microseconds once decoded — it is **hiding seek latency**: these
/// datasets sit on a rotational disk, and concurrent reads let the drive
/// queue and reorder seeks instead of serialising them one file at a time.
/// Measured with `RAYON_NUM_THREADS=1` against the default (this exact code
/// path both times, so the comparison is real, not a different serial
/// implementation) on cold, previously unscanned directories: **15.0 ms per
/// file serial versus 6.6 ms per file parallel — 2.3x**. An SSD or a warm
/// page cache sees much less of this (the fixture corpus, cached after
/// Phase 4's own test runs, shows no measurable difference at all), which is
/// why this is not "faster on every machine" so much as "never slower, and
/// sometimes much faster."
pub fn extract_from_directories(paths: &[String]) -> Result<Table> {
    let files = scan_directories(paths)?;
    let results: Vec<Option<Record>> = files
        .par_iter()
        .map(|p| match extract_single_file(p) {
            Ok(r) => Some(r),
            Err(e) => {
                // "Silent failure for individual files to prevent pipeline
                // crashing" -- the Python logs and drops the row. Printing
                // directly from a worker thread is safe (each `eprintln!`
                // call takes the stream lock for its one write) but the
                // interleaving of multiple files' warnings is unspecified,
                // unlike the Python's single-process log.
                eprintln!("warning: error parsing headers for {p}: {e}");
                None
            }
        })
        .collect();
    let records: Vec<Record> = results.into_iter().flatten().collect();
    Ok(Table::from_records(&records))
}

/// The scan order every downstream "first wins" resolution depends on
/// (PORT_PLAN.md hazard 8).
///
/// Three details are load-bearing and none of them is what a Rust directory
/// walker does by default:
///
/// - the path is `os.path.join(root, file)` built from the **CLI argument as
///   given** — relative stays relative, and it is never canonicalised;
/// - every input directory's files go into **one list** before sorting, so
///   directories interleave rather than concatenating;
/// - the sort is Python's `str` sort, i.e. by Unicode scalar value, which for
///   UTF-8 is a plain bytewise sort — `Vec<String>::sort` is exactly that.
///
/// `os.walk(..., followlinks=True)` follows symlinked directories.
pub fn scan_directories(paths: &[String]) -> Result<Vec<String>> {
    let mut files: Vec<String> = Vec::new();
    for path in paths {
        walk(path, &mut files)
            .with_context(|| format!("scanning directory: {path}"))?;
    }
    files.sort();
    Ok(files)
}

fn walk(dir: &str, out: &mut Vec<String>) -> Result<()> {
    let entries = std::fs::read_dir(dir).with_context(|| format!("reading {dir}"))?;
    let mut subdirs: Vec<String> = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let joined = pathutil::join(dir, &name);
        // `os.walk` classifies by the *followed* type, so a symlink to a
        // directory is a directory here.
        let meta = match std::fs::metadata(&joined) {
            Ok(m) => m,
            // A broken symlink is neither a file nor a directory to os.walk.
            Err(_) => continue,
        };
        if meta.is_dir() {
            subdirs.push(joined);
        } else if has_scan_extension(&name) {
            out.push(joined);
        }
    }
    // The traversal order does not matter -- everything is sorted afterwards
    // -- but recursing in a defined order keeps the warning stream stable.
    subdirs.sort();
    for sub in subdirs {
        walk(&sub, out)?;
    }
    Ok(())
}

fn has_scan_extension(name: &str) -> bool {
    let lower = name.to_lowercase();
    SCAN_EXTENSIONS.iter().any(|e| lower.ends_with(e))
}

/// `extract_single_file`: dispatch on extension, strip quotes, then add the
/// three fields the readers do not come up with themselves.
pub fn extract_single_file(path: &str) -> Result<Record> {
    let lower = path.to_lowercase();
    let mut hdr = if FITS_EXTENSIONS.iter().any(|e| lower.ends_with(e)) {
        crate::fits::read_fits(path)?
    } else if lower.ends_with(".xisf") {
        crate::xisf::read_xisf(path)?
    } else {
        anyhow::bail!("unsupported file format");
    };

    let mut cleaned = Record::default();
    for key in &hdr.keys {
        let value = hdr.values.remove(key).expect("key came from keys");
        cleaned.insert(key, value.strip_quotes());
    }

    // Set after the quote-strip: it is a path, not raw header text.
    cleaned.insert(
        col::SOURCE_PATH_RAW,
        HeaderValue::Str(pathutil::abspath(path)),
    );
    Ok(cleaned)
}

/// `_get_fit_number`: a PixInsight master records how many sub-exposures went
/// into it in a `HISTORY` line, and that count becomes the frame's `NUMBER`.
/// Anything else is a single frame.
pub fn fit_number(hdr: &Record) -> i64 {
    let Some(HeaderValue::Commentary(lines)) = hdr.get("HISTORY") else {
        return 1;
    };
    for line in lines {
        if line.contains("ImageIntegration.numberOfImages:") {
            if let Some((_, tail)) = line.rsplit_once(':') {
                if let Ok(n) = tail.trim().parse::<i64>() {
                    return n;
                }
            }
        }
    }
    1
}

impl Table {
    /// `pd.DataFrame(list_of_dicts)`.
    ///
    /// Not `read_csv` inference, and the differences are silent ones.
    /// Measured on pandas 2.2.3:
    ///
    /// | Records | dtype |
    /// |---|---|
    /// | `1`, `2` | `int64` |
    /// | `1`, *key absent* | `float64` (missing → NaN) |
    /// | `1`, `2.5` | `float64` |
    /// | `True`, `False` | `bool` |
    /// | `True`, *key absent* | **`object`** |
    /// | `True`, `2` | `object` |
    /// | `"x"`, anything | `object` |
    /// | `10**20`, `1` | `object` (beyond i64) |
    /// | `None`, `1` | `float64` — `None` is a missing *value* |
    /// | `"None"`, `"x"` | `object` — and it stays the **string** `None`, |
    ///
    /// where `read_csv` would have made that last one an NA sentinel. That
    /// one difference is why `[defaults]`' literal `None` survives a scan but
    /// not a `--test` replay.
    ///
    /// Column order is first appearance across the records, in scan order.
    pub fn from_records(records: &[Record]) -> Table {
        let mut names: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for r in records {
            for k in &r.keys {
                if seen.insert(k.as_str()) {
                    names.push(k.clone());
                }
            }
        }

        let n_rows = records.len();
        let columns = names
            .into_iter()
            .map(|name| {
                let values: Vec<Option<&HeaderValue>> =
                    records.iter().map(|r| r.get(&name)).collect();
                let dtype = infer_record_dtype(&values);
                let cells = values
                    .iter()
                    .map(|v| match v {
                        None => Cell::Null,
                        Some(hv) => coerce_record(hv, dtype),
                    })
                    .collect();
                Column { name, dtype, cells }
            })
            .collect();

        Table { columns, n_rows }
    }
}

fn infer_record_dtype(values: &[Option<&HeaderValue>]) -> DType {
    let mut has_missing = false;
    let (mut ints, mut floats, mut bools, mut strs) = (0usize, 0usize, 0usize, 0usize);
    for v in values {
        match v {
            None => has_missing = true,
            Some(HeaderValue::Int(_)) => ints += 1,
            Some(HeaderValue::Float(_)) => floats += 1,
            Some(HeaderValue::Bool(_)) => bools += 1,
            Some(HeaderValue::Str(_)) | Some(HeaderValue::Commentary(_)) => strs += 1,
        }
    }
    if strs > 0 {
        return DType::Str;
    }
    if bools > 0 {
        // A bool column survives as `bool` only if it is nothing but bools.
        return if has_missing || ints > 0 || floats > 0 {
            DType::Str
        } else {
            DType::Bool
        };
    }
    if floats > 0 || has_missing {
        // Missing demotes an integer column to float64, exactly as in the
        // CSV path -- there is no integer NaN.
        return if ints + floats == 0 {
            DType::Float // every value was None: an all-NaN float column
        } else {
            DType::Float
        };
    }
    if ints > 0 {
        DType::Int
    } else {
        DType::Float
    }
}

fn coerce_record(v: &HeaderValue, dtype: DType) -> Cell {
    match (v, dtype) {
        (HeaderValue::Int(i), DType::Float) => Cell::Float(*i as f64),
        // An `object` column keeps the Python objects themselves; the dump
        // renders them with `str()`, which is what `astype_str` reproduces.
        (other, DType::Str) => Cell::Str(crate::steps::astype_str(&other.to_cell())),
        (other, _) => other.to_cell(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(pairs: &[(&str, HeaderValue)]) -> Record {
        let mut r = Record::default();
        for (k, v) in pairs {
            r.insert(k, v.clone());
        }
        r
    }

    #[test]
    fn column_order_is_first_appearance_across_records() {
        let t = Table::from_records(&[
            rec(&[("z", HeaderValue::Int(1)), ("a", HeaderValue::Int(2))]),
            rec(&[("m", HeaderValue::Int(3)), ("a", HeaderValue::Int(4))]),
            rec(&[("b", HeaderValue::Int(5))]),
        ]);
        let names: Vec<&str> = t.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["z", "a", "m", "b"]);
    }

    /// Every row measured against `pd.DataFrame(records)` on pandas 2.2.3.
    #[test]
    fn record_inference_is_not_read_csv_inference() {
        let d = |recs: &[Record]| Table::from_records(recs).columns[0].dtype;
        let a = |v: HeaderValue| rec(&[("a", v)]);
        let other = || rec(&[("b", HeaderValue::Int(9))]);

        assert_eq!(d(&[a(HeaderValue::Int(1)), a(HeaderValue::Int(2))]), DType::Int);
        assert_eq!(d(&[a(HeaderValue::Int(1)), other()]), DType::Float);
        assert_eq!(d(&[a(HeaderValue::Int(1)), a(HeaderValue::Float(2.5))]), DType::Float);
        assert_eq!(d(&[a(HeaderValue::Bool(true)), a(HeaderValue::Bool(false))]), DType::Bool);
        // bool + missing is object, not bool -- there is no nullable bool here.
        assert_eq!(d(&[a(HeaderValue::Bool(true)), other()]), DType::Str);
        assert_eq!(d(&[a(HeaderValue::Bool(true)), a(HeaderValue::Int(2))]), DType::Str);
        assert_eq!(d(&[a(HeaderValue::Str("x".into())), other()]), DType::Str);
    }

    /// The difference that catches people: on this path `None` is a string.
    #[test]
    fn the_literal_string_none_survives_a_scan() {
        let t = Table::from_records(&[
            rec(&[("a", HeaderValue::Str("None".into()))]),
            rec(&[("a", HeaderValue::Str("x".into()))]),
        ]);
        assert_eq!(t.columns[0].dtype, DType::Str);
        assert_eq!(t.columns[0].cells[0], Cell::Str("None".into()));
    }

    #[test]
    fn quotes_are_stripped_from_strings_but_not_from_commentary() {
        assert_eq!(
            HeaderValue::Str("'Ha'".into()).strip_quotes(),
            HeaderValue::Str("Ha".into())
        );
        assert_eq!(
            HeaderValue::Str("\"Quoted Target\"".into()).strip_quotes(),
            HeaderValue::Str("Quoted Target".into())
        );
        let c = HeaderValue::Commentary(vec!["'not stripped'".into()]);
        assert_eq!(c.clone().strip_quotes(), c);
    }

    #[test]
    fn the_master_frame_count_comes_out_of_a_history_line() {
        let mut r = Record::default();
        r.insert(
            "HISTORY",
            HeaderValue::Commentary(vec![
                "ImageIntegration.totalPixels: 244735104".into(),
                "ImageIntegration.numberOfImages: 50".into(),
            ]),
        );
        assert_eq!(fit_number(&r), 50);
        assert_eq!(fit_number(&Record::default()), 1);
    }

    #[test]
    fn the_fz_extension_is_not_scanned() {
        assert!(has_scan_extension("a.fits"));
        assert!(has_scan_extension("A.FITS"));
        assert!(has_scan_extension("a.xisf"));
        assert!(has_scan_extension("a.fit"));
        assert!(has_scan_extension("a.fts"));
        // The conventional tile-compressed spelling, silently skipped.
        assert!(!has_scan_extension("a.fits.fz"));
    }
}

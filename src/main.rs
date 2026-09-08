//! Rust port of AstroBinUpload.py — Phases 1–3.
//!
//! Parity target: Python `v2.1.1` (see PORT_PLAN.md). Landed: the CLI, the
//! configobj-compatible config parser, the `--test` CSV ingest path with
//! pandas-equivalent dtype inference, the six pipeline steps, and the
//! exporter plus reports. Still missing: the FITS/XISF readers (Phase 4), so
//! a run without `--test` has nothing to scan and says so rather than
//! pretending otherwise.

mod appconfig;
mod cli;
mod config;
mod datetime;
mod constants;
mod dump;
mod exporter;
mod extractor;
mod fits;
mod numeric;
mod pathutil;
mod pandas_fmt;
mod reports;
mod steps;
mod table;
mod xisf;

use anyhow::{bail, Context, Result};
use clap::Parser;

use crate::cli::Cli;
use crate::config::ConfigFile;
use crate::table::Table;

fn main() -> Result<()> {
    let args = Cli::parse();

    // B10 in REMEDIATION_PLAN.md: the Python side validates directory
    // arguments before use, because an unvalidated typo reached
    // os.makedirs() and silently created the typo'd tree.
    for dir in &args.directory_paths {
        if !dir.exists() {
            bail!("directory path does not exist: {}", dir.display());
        }
        if !dir.is_dir() {
            bail!("path is not a directory: {}", dir.display());
        }
    }

    // Unlike Python, a missing config is an error rather than a prompt to
    // generate a template: template generation is an interactive convenience
    // that has no place in a binary the differential harness drives.
    if !args.config.exists() {
        bail!(
            "configuration file not found: {} (the Rust port does not \
             auto-generate one; use the Python entry point for that)",
            args.config.display()
        );
    }
    let cfg = ConfigFile::parse_file(&args.config)
        .with_context(|| format!("parsing {}", args.config.display()))?;

    if args.dump_parity {
        dump_parity(&cfg, args.test.as_deref())?;
        return Ok(());
    }

    if args.dump_steps {
        dump_steps(&cfg, &args)?;
        return Ok(());
    }

    if args.dump_report_stats {
        dump_report_stats(&cfg, &args)?;
        return Ok(());
    }

    // The output directory lives inside the *resolved* first argument, but
    // the basename the artifacts are named after comes from the raw argument
    // string, before any resolution -- `os.path.basename(args.directory_paths[0])`,
    // not of the abspath'd copy.
    let first = args.directory_paths[0].to_string_lossy().into_owned();
    // `os.path.abspath`, not `canonicalize`: the Python normalises the string
    // and leaves symlinks alone, and the same rule writes `SOURCE_PATH`, whose
    // `dirname` is half of DeduplicateStep's group key.
    let out_dir_str = pathutil::join(&pathutil::abspath(&first), "AstroBinUploadInfo");
    let out_dir = std::path::PathBuf::from(&out_dir_str);
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("creating {}", out_dir.display()))?;

    let basename = pathutil::basename(&first).replace(' ', "_");

    println!("Output directory: {}", out_dir.display());
    // The "legacy-compliant console boot sequence" main() prints verbatim.
    // `utils version` names a module that has not existed since v2.0 and
    // still reports the application's own version; it is echoed rather than
    // corrected, because stdout is part of what parity means here.
    println!("Logging initialized.");
    println!("main version: {}", env!("CARGO_PKG_VERSION"));
    println!("utils version: {}", env!("CARGO_PKG_VERSION"));

    let raw = load_headers(&args, true)?;

    // Written only on the scan path: the export sits in the `else` branch of
    // `if args.test`, so an injected run -- which was fed one of these files
    // in the first place -- does not rewrite it.
    if args.debug && args.test.is_none() && raw.n_rows > 0 {
        let path = pathutil::join(&out_dir_str, "debug_step_00_RawHeaders.csv");
        std::fs::write(&path, exporter::to_csv(&raw))
            .with_context(|| format!("writing {path}"))?;
    }

    let app = crate::appconfig::AppConfig::from_config(&cfg)?;
    let agg = match run_pipeline(&raw, &app, Some(out_dir_str.as_str()), args.debug) {
        Ok(agg) => agg,
        Err(e) => {
            // main()'s final safety net. The dump is deliberately non-fatal
            // -- it runs while the program is already dying and must never
            // itself raise -- and `--test` is not excluded here, unlike the
            // step-00 export above.
            if raw.n_rows > 0 {
                let path = pathutil::join(&out_dir_str, "emergency_raw_dump.csv");
                match std::fs::write(&path, exporter::to_csv(&raw)) {
                    Ok(()) => println!("Emergency data dump saved to: {path}"),
                    Err(err) => eprintln!("Emergency data dump also failed: {err}"),
                }
            }
            return Err(e);
        }
    };

    let now = local_timestamp();
    if let Some(summary) = exporter::export(&agg, raw.n_rows, &basename, &out_dir, &now)? {
        // `print(summary)` on the Python side.
        println!("{summary}");
    }
    println!("\nProcessing complete.");
    Ok(())
}

/// The raw header frame: injected from a CSV, or scanned off disk.
///
/// The two build their frames by different rules -- see `extractor.rs`'s
/// table -- so which one ran is visible in the result, not just in how it got
/// there.
fn load_headers(args: &Cli, announce: bool) -> Result<Table> {
    if announce {
        // Printed before the `if args.test` branch on the Python side, so an
        // injected run announces the read it is not doing too. Suppressed in
        // the dump paths: their output *is* stdout.
        println!("\nReading FITS headers...\n");
    }
    match args.test.as_deref() {
        Some(csv) => Table::read_csv_upper(csv)
            .with_context(|| format!("ingesting {}", csv.display())),
        None => {
            let paths: Vec<String> = args
                .directory_paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            extractor::extract_from_directories(&paths, announce)
        }
    }
}

/// The six steps, in the order `PipelineProcessor` runs them.
///
/// `out_dir` and `debug` mirror `PipelineProcessor.run(debug=..., output_dir=...)`:
/// with `--debug`, each step's result is written to
/// `debug_step_NN_<StepName>.csv`; on a failure the state the failing step was
/// *handed* is written to `..._CRASH_DIAGNOSTIC.csv` whether or not `--debug`
/// was given, because `state = step.execute(state)` never assigns when
/// `execute` raises. The names are the Python *class* names -- that is what
/// `_dump_debug_csv` interpolates (`step.__class__.__name__`), and the
/// filenames are what `--test` consumes afterwards.
fn run_pipeline(
    raw: &Table,
    app: &crate::appconfig::AppConfig,
    out_dir: Option<&str>,
    debug: bool,
) -> Result<Table> {
    // `_dump_debug_csv`'s own three-way priority: the aggregated frame at the
    // aggregation step, else the processed frame, else -- at step 1 only --
    // the raw frame. When every candidate is empty it writes nothing at all,
    // rather than a header-only file.
    let dump = |i: usize, name: &str, suffix: &str, candidates: &[&Table]| {
        let Some(dir) = out_dir else { return };
        let Some(t) = debug_dump_target(candidates) else { return };
        let path = pathutil::join(dir, &format!("debug_step_{i:02}_{name}{suffix}.csv"));
        // `_dump_debug_csv` swallows its own failures ("Failed to save debug
        // CSV for {step}") rather than killing a run that was otherwise fine.
        if let Err(e) = std::fs::write(&path, exporter::to_csv(t)) {
            eprintln!("Failed to save debug CSV for {name}: {e}");
        }
    };

    macro_rules! step {
        ($i:expr, $name:expr, $input:expr, $call:expr) => {
            match $call {
                Ok(out) => {
                    if debug {
                        dump($i, $name, "", &[&out]);
                    }
                    out
                }
                Err(e) => {
                    dump($i, $name, "_CRASH_DIAGNOSTIC", &[$input, raw]);
                    return Err(e);
                }
            }
        };
    }

    let df = step!(1, "NormalizeHeadersStep", raw, steps::normalize::execute(raw, app));
    let df = step!(2, "OpticalParameterStep", &df, steps::optical::execute(&df, app));
    let df = step!(3, "DeduplicateStep", &df, steps::deduplicate::execute(&df));
    let df = step!(4, "CalibrationMatcherStep", &df, steps::calibration::execute(&df));
    let df = step!(5, "GeocodeStep", &df, steps::geocode::execute(&df, app));

    // The aggregation step is the one place the dump prefers a different
    // frame: `aggregated_df` when it has rows, falling back to the frame that
    // went in when it does not.
    match steps::aggregate::execute(&df, app) {
        Ok(agg) => {
            if debug {
                dump(6, "AggregationStep", "", &[&agg, &df]);
            }
            Ok(agg)
        }
        Err(e) => {
            dump(6, "AggregationStep", "_CRASH_DIAGNOSTIC", &[&df, raw]);
            Err(e)
        }
    }
}

/// `_dump_debug_csv`'s frame choice: the first candidate that has rows, in
/// the priority order the caller lists them, and `None` when every one is
/// empty -- Python falls off the end of its `if`/`elif` chain there and
/// writes no file at all, rather than a header-only one.
fn debug_dump_target<'a>(candidates: &[&'a Table]) -> Option<&'a Table> {
    candidates.iter().copied().find(|t| t.n_rows > 0)
}

/// The summary's temperature statistics, one line per site, as raw IEEE-754
/// bits — the same little-endian hex `dump.rs` uses for float cells.
fn dump_report_stats(cfg: &ConfigFile, args: &Cli) -> Result<()> {
    let raw = load_headers(args, false)?;
    let app = crate::appconfig::AppConfig::from_config(cfg)?;
    let agg = run_pipeline(&raw, &app, None, false)?;
    for (site, st) in reports::report_temp_stats(&agg) {
        println!(
            "SITE\t{site}\t{}\t{}\t{}\t{}",
            st.count,
            dump::canon(&crate::table::Cell::Float(st.min)),
            dump::canon(&crate::table::Cell::Float(st.max)),
            dump::canon(&crate::table::Cell::Float(st.mean)),
        );
    }
    Ok(())
}

/// `datetime.now().strftime("%Y-%m-%d %H:%M:%S")` in the local timezone.
///
/// The one line of output that legitimately differs between runs, and the one
/// the golden harness normalises away. Implemented against `libc::localtime`
/// through `chrono` rather than by hand: a UTC-only clock would produce a
/// summary that reads wrong to the user in every timezone but one.
fn local_timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Canonical, line-oriented dump of everything Phase 1 parses.
///
/// Deliberately boring and sorted so it can be diffed byte-for-byte against
/// the equivalent dump produced from configobj and pandas.
fn dump_parity(cfg: &ConfigFile, test_csv: Option<&std::path::Path>) -> Result<()> {
    use crate::config::{Section, Value};

    fn render(v: &Value) -> String {
        match v {
            Value::Str(s) => format!("str\t{s}"),
            Value::List(items) => format!("list\t{}", items.join("\u{1f}")),
        }
    }

    fn walk(prefix: &str, sec: &Section, out: &mut Vec<String>) {
        for (k, v) in &sec.values {
            out.push(format!("{prefix}\t{k}\t{}", render(v)));
        }
        for (name, sub) in &sec.sections {
            walk(&format!("{prefix}/{name}"), sub, out);
        }
    }

    let mut lines = Vec::new();
    for (name, sec) in &cfg.sections {
        walk(name, sec, &mut lines);
    }
    lines.sort();
    for l in &lines {
        println!("CONFIG\t{l}");
    }

    if let Some(csv) = test_csv {
        let t = Table::read_csv_upper(csv)?;
        println!("CSV\trows\t{}", t.n_rows);
        let mut cols: Vec<String> = t
            .columns
            .iter()
            .map(|c| {
                let d = crate::dump::dtype_tag(c.dtype);
                let nulls = c.cells.iter().filter(|x| x.is_null()).count();
                format!("CSV\tcol\t{}\t{d}\t{nulls}", c.name)
            })
            .collect();
        cols.sort();
        for c in cols {
            println!("{c}");
        }
    }
    Ok(())
}

/// Per-step dump, diffed line-for-line against `parity/dump_steps.py`.
///
/// Emits `00_raw` first, then one block per implemented step, so a mismatch
/// localises to the first step that diverges. Steps not yet ported simply do
/// not appear -- the diff is taken over the prefix both sides emit.
fn dump_steps(cfg: &ConfigFile, args: &Cli) -> Result<()> {
    use std::io::Write;

    let raw = load_headers(args, false)?;
    let app = crate::appconfig::AppConfig::from_config(cfg)?;

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    dump::dump_frame("00_raw", &raw, &mut out)?;

    let df = steps::normalize::execute(&raw, &app)?;
    dump::dump_frame("01_NormalizeHeadersStep", &df, &mut out)?;

    let df = steps::optical::execute(&df, &app)?;
    dump::dump_frame("02_OpticalParameterStep", &df, &mut out)?;

    let df = steps::deduplicate::execute(&df)?;
    dump::dump_frame("03_DeduplicateStep", &df, &mut out)?;

    let df = steps::calibration::execute(&df)?;
    dump::dump_frame("04_CalibrationMatcherStep", &df, &mut out)?;

    let df = steps::geocode::execute(&df, &app)?;
    dump::dump_frame("05_GeocodeStep", &df, &mut out)?;

    // The oracle switches frames for the last step: `AggregationStep` emits
    // the aggregated frame, not the processed one it was built from.
    let agg = steps::aggregate::execute(&df, &app)?;
    dump::dump_frame("06_AggregationStep", &agg, &mut out)?;

    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(n_rows: usize) -> Table {
        Table {
            columns: Vec::new(),
            n_rows,
        }
    }

    /// `_dump_debug_csv`'s `if`/`elif` chain, including the branch nothing in
    /// the corpus reaches: when every candidate frame is empty it writes no
    /// file, so an empty run leaves no `debug_step_NN_*.csv` behind rather
    /// than a file holding only a header row.
    #[test]
    fn the_debug_dump_takes_the_first_non_empty_frame_and_none_when_all_are_empty() {
        let full = table(3);
        let other = table(7);
        let empty = table(0);

        assert_eq!(debug_dump_target(&[&full, &other]).unwrap().n_rows, 3);
        assert_eq!(debug_dump_target(&[&empty, &other]).unwrap().n_rows, 7);
        assert!(debug_dump_target(&[&empty, &empty]).is_none());
        assert!(debug_dump_target(&[]).is_none());
    }
}

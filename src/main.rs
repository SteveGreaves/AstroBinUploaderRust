//! Rust port of AstroBinUpload.py.
//!
//! Parity target: Python `v2.2.0`/`v2.2.1` (see PORT_PLAN.md). Landed: the
//! CLI, the configobj-compatible config parser, the `--test` CSV ingest path
//! with pandas-equivalent dtype inference, the FITS/XISF readers, the six
//! pipeline steps, the exporter plus reports, the first-run config bootstrap,
//! the `[secret]`-gated site-lookup network layer, and `main`'s fatal-error
//! net (`fatal_error`, below).

mod appconfig;
mod cli;
mod config;
mod config_write;
mod logging;
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
mod sites;
mod steps;
mod table;
mod xisf;

use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser};

use crate::cli::Cli;
use crate::config::ConfigFile;
use crate::table::Table;

fn main() -> Result<()> {
    let args = Cli::parse();

    // Step 0 -- first-run configuration bootstrap (AstroBinUpload.py's
    // `main`, restored in v2.2.0). Running with no directory paths at all
    // generates a default config.ini and exits, so a brand-new user has
    // something to edit. Runs before any path-dependent setup, exactly as
    // on the Python side; see PORT_PLAN.md Phase 7C.
    if args.directory_paths.is_empty() {
        if args.config.exists() {
            println!(
                "\nNo directory path provided, and '{}' already exists.\n\
                 Give one or more directories to scan.\n",
                args.config.display()
            );
            // The usage line clap renders here collapses every flag to
            // `[OPTIONS]` rather than enumerating them across wrapped lines.
            // Reproducing argparse's formatter for one line isn't worth it,
            // so the line is simply this program's own.
            print!("{}", Cli::command().render_usage());
            println!();
            std::process::exit(1);
        }
        if args.config != std::path::Path::new("config.ini") {
            // Only the default name is ever generated; a named profile
            // that is missing is a mistake, not a request to create one.
            println!(
                "\nThe specified configuration file '{}' was not found.\n",
                args.config.display()
            );
            std::process::exit(1);
        }
        generate_default_config(&args.config)?;
        std::process::exit(0);
    }

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

    if args.dump_parity {
        let cfg = load_config(&args)?;
        dump_parity(&cfg, args.test.as_deref())?;
        return Ok(());
    }

    if args.dump_steps {
        let cfg = load_config(&args)?;
        dump_steps(&cfg, &args)?;
        return Ok(());
    }

    if args.dump_report_stats {
        let cfg = load_config(&args)?;
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

    // Opened before the configuration is read, so a configuration error is
    // itself logged -- which is why `load_config` is called below this rather
    // than with the dump paths above.
    let log_file = out_dir.join("AstroBinUploader.log");
    logging::init(&log_file, args.debug);
    log_info!("main", 240, "Logging initialized.");
    log_info!("main", 245, "main version: {}", env!("CARGO_PKG_VERSION"));
    log_info!("main", 246, "utils version: {}", env!("CARGO_PKG_VERSION"));
    // `sys.argv` rendered as Python renders a list of strings. It can only
    // ever be *this* program's argv, so argv[0] is the binary rather than a
    // .py file; the shape of the line is what matches, not its first element.
    log_info!(
        "main",
        247,
        "Calling function and arguments provided: [{}]",
        std::env::args()
            .map(|a| format!("'{a}'"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    log_info!("main", 248, "");

    println!("Output directory: {}", out_dir.display());
    // The "legacy-compliant console boot sequence" main() prints verbatim.
    // `utils version` names a module that has not existed since v2.0 and
    // still reports the application's own version; it is echoed rather than
    // corrected, because stdout is part of what parity means here.
    println!("Logging initialized.");
    println!("main version: {}", env!("CARGO_PKG_VERSION"));
    println!("utils version: {}", env!("CARGO_PKG_VERSION"));

    // Everything from here to `Processing complete.` is Python's `try:` block
    // (`AstroBinUpload.py` main, Step 2-4): `loader.load`, the header read and
    // its `--debug` step-00 export, `processor.run`, and `exporter.export`. A
    // failure at any point reaches the same `except Exception` net -- the
    // `main:315`/`316` records, the emergency dump, the two console lines and
    // `sys.exit(1)` -- so each fallible call here routes to `fatal_error`
    // rather than propagating out of `main` via `?`. The emergency dump only
    // fires once `raw` is bound and non-empty, matching Python's
    // `'raw_df' in locals() and not raw_df.empty`.
    let cfg = match load_config(&args) {
        Ok(cfg) => cfg,
        Err(e) => fatal_error(&e, None, &out_dir_str, &log_file),
    };
    // Built here rather than after extraction: the Python `AppConfig(...)`
    // constructor runs inside `ConfigLoader.load`, so the warnings its
    // normalisers emit land before the scan's records, not after them -- and,
    // for the same reason, a failure to build it is a `loader.load` failure,
    // reached before `raw_df` exists, so no emergency dump.
    let app = match crate::appconfig::AppConfig::from_config(&cfg) {
        Ok(app) => app,
        Err(e) => fatal_error(&e, None, &out_dir_str, &log_file),
    };
    let raw = match load_headers(&args, true, Some(out_dir_str.as_str())) {
        Ok(raw) => raw,
        // `raw_df = extractor.extract_*(...)` has not completed assigning, so
        // Python's `'raw_df' in locals()` is false here too: no dump.
        Err(e) => fatal_error(&e, None, &out_dir_str, &log_file),
    };

    // Written only on the scan path: the export sits in the `else` branch of
    // `if args.test`, so an injected run -- which was fed one of these files
    // in the first place -- does not rewrite it.
    if args.debug && args.test.is_none() && raw.n_rows > 0 {
        let path = pathutil::join(&out_dir_str, "debug_step_00_RawHeaders.csv");
        // `raw` is bound by now, so a failure of this write *does* reach the
        // emergency dump on the Python side (`raw_df` is in `locals()`).
        if let Err(e) =
            std::fs::write(&path, exporter::to_csv(&raw)).with_context(|| format!("writing {path}"))
        {
            fatal_error(&e, Some(&raw), &out_dir_str, &log_file);
        }
        log_info!("main", 277, "Raw scanned headers exported to {path}");
    }

    // The network layer. `SiteLookup::new` reads `[secret]`; with no such
    // section `enabled` is false and no socket is ever opened, which is the
    // ordinary case and what keeps the golden corpus offline.
    //
    // `config_path` is None on the --test replay path -- `SessionState(
    // config_path=None if args.test else args.config)` -- so a diagnostic
    // run never edits the user's configuration. Note it does not stop the
    // *lookups*: Python gates only the write, and that is reproduced.
    let transport = crate::sites::UreqTransport;
    let lookup = crate::sites::SiteLookup::new(&transport, &app);
    let config_path = if args.test.is_some() {
        None
    } else {
        Some(args.config.as_path())
    };

    let agg = match run_pipeline(
        &raw,
        &app,
        Some(out_dir_str.as_str()),
        args.debug,
        Some(&lookup),
        config_path,
    ) {
        Ok(agg) => agg,
        Err(e) => fatal_error(&e, Some(&raw), &out_dir_str, &log_file),
    };

    let now = local_timestamp();
    let summary = match exporter::export(&agg, raw.n_rows, &basename, &out_dir, &now) {
        Ok(summary) => summary,
        Err(e) => fatal_error(&e, Some(&raw), &out_dir_str, &log_file),
    };
    if let Some(summary) = summary {
        // `print(summary)` on the Python side.
        println!("{summary}");
    }
    println!("\nProcessing complete.");
    Ok(())
}

/// The Python `try:`/`except Exception` net around `main`'s Step 2-4
/// (`AstroBinUpload.py`). `loader.load`, the header read, `processor.run` or
/// `exporter.export` failing all land here:
///
/// * `logger.error(...)` / `logger.exception(e)` -- `main:315`/`316`. The
///   port logs only `e`'s outermost message where Python's `logger.exception`
///   also writes a traceback; that half is unmatchable by construction (the
///   two stacks are different languages) and always has been.
/// * the emergency dump, but only when `raw` is `Some` and non-empty --
///   Python guards it on `'raw_df' in locals() and not raw_df.empty`, so a
///   failure before the scan completes writes nothing. Non-fatal: it runs
///   while the program is already exiting and must not itself raise.
/// * `print(f"\n[CRITICAL ERROR]: {str(e)}")` and the `Detailed diagnostics`
///   line naming the log file, then `sys.exit(1)`.
///
/// `str(e)` is `e`'s outermost message; `{e}` matches that (not `{e:#}`,
/// which would append the whole `anyhow` context chain). Where a call above
/// wraps its error in `.with_context(...)` -- the config *parse* path, the
/// reader path -- `{e}` is that context string rather than Python's
/// underlying library message; those paths are reachable only by a corrupt
/// config or an unreadable file, no fixture exercises them, and exception
/// text was never part of the parity claim.
fn fatal_error(
    e: &anyhow::Error,
    raw: Option<&Table>,
    out_dir_str: &str,
    log_file: &std::path::Path,
) -> ! {
    log_error!(
        "main",
        315,
        "The application encountered a fatal error and must exit."
    );
    log_error!("main", 316, "{e}");
    if let Some(raw) = raw {
        if raw.n_rows > 0 {
            let path = pathutil::join(out_dir_str, "emergency_raw_dump.csv");
            match std::fs::write(&path, exporter::to_csv(raw)) {
                Ok(()) => println!("Emergency data dump saved to: {path}"),
                // Python only `logger.debug`s this -- no console line.
                Err(err) => log_debug!("main", 329, "Emergency data dump also failed: {err}"),
            }
        }
    }
    println!("\n[CRITICAL ERROR]: {e}");
    println!(
        "Detailed diagnostics have been saved to: {}",
        log_file.display()
    );
    std::process::exit(1);
}

/// `ConfigLoader.load`.
///
/// Reachable with a missing `config.ini` even when directory paths *were*
/// given: Python's own generation branch lives here, inside `load`, not
/// only in `main`'s Step-0 bootstrap -- so a user who deletes `config.ini`
/// and reruns with real arguments gets a freshly generated template and a
/// clean exit(0), same as an argument-less first run, except that logging
/// is already initialised by the time this fires, so the
/// `config.ini missing...` record actually lands (loader.py:62; the Step-0
/// bootstrap's call to the same generator runs before a log sink exists,
/// matching Python's own logger-has-no-handlers-yet comment).
fn load_config(args: &Cli) -> Result<ConfigFile> {
    if !args.config.exists() {
        if args.config == std::path::Path::new("config.ini") {
            generate_default_config(&args.config)?;
            std::process::exit(0);
        }
        log_error!(
            "load",
            68,
            "Custom configuration file missing: {}",
            args.config.display()
        );
        // Text matches Python's own `FileNotFoundError` message. This `Err`
        // is routed through `fatal_error` by `main`'s call site -- Python's
        // `try:` wraps `loader.load(...)`, so a missing custom config reaches
        // the `main:315`/`316` records, the `[CRITICAL ERROR]` / `Detailed
        // diagnostics` console lines and `exit(1)` (no emergency dump: no
        // `raw_df` yet). Verified byte-for-byte against live Python 2026-09-10.
        bail!(
            "The specified configuration file '{}' was not found.",
            args.config.display()
        );
    }
    let cfg = ConfigFile::parse_file(&args.config)
        .with_context(|| format!("parsing {}", args.config.display()))?;
    log_info!(
        "load",
        81,
        "Configuration loaded and normalized from {}",
        args.config.display()
    );
    Ok(cfg)
}

/// `ConfigLoader.load`'s generation branch (loader.py:61-65), shared by
/// `main`'s Step-0 bootstrap (no directory paths; logger not yet
/// initialised, so this call's `log_info!` is a silent no-op, matching
/// Python's unhandled logger there) and `load_config` above (directories
/// given, but the default `config.ini` happens to be missing -- logging
/// already initialised, so the record lands for real).
fn generate_default_config(path: &std::path::Path) -> Result<()> {
    log_info!(
        "load",
        62,
        "config.ini missing. Generating default configuration template."
    );
    config_write::write_default_config(path)
        .with_context(|| format!("writing generated config to {}", path.display()))?;
    println!(
        "\nA new {} file was created. Please edit this before re-running the script.",
        path.display()
    );
    Ok(())
}

/// The raw header frame: injected from a CSV, or scanned off disk.
///
/// The two build their frames by different rules -- see `extractor.rs`'s
/// table -- so which one ran is visible in the result, not just in how it got
/// there.
fn load_headers(args: &Cli, announce: bool, out_dir: Option<&str>) -> Result<Table> {
    if announce {
        // Printed before the `if args.test` branch on the Python side, so an
        // injected run announces the read it is not doing too. Suppressed in
        // the dump paths: their output *is* stdout.
        println!("\nReading FITS headers...\n");
    }
    match args.test.as_deref() {
        Some(csv) => {
            // `resolve_test_csv` is the real pipeline's behaviour
            // (AstroBinUpload.py's `main`); the hidden dump_* paths pass
            // `out_dir: None` and read the given path directly, as before --
            // they are not part of Python's CLI surface, so they have no
            // `output_dir` to resolve against and no reason to grow one.
            let resolved = match out_dir {
                Some(out_dir) => resolve_test_csv(&csv.to_string_lossy(), out_dir),
                None => csv.to_string_lossy().into_owned(),
            };
            log_info!(
                "extract_from_csv",
                148,
                "Injecting metadata from CSV: {}",
                resolved
            );
            Table::read_csv_upper(std::path::Path::new(&resolved))
                .with_context(|| format!("ingesting {resolved}"))
        }
        None => {
            // `[os.path.abspath(os.path.expanduser(p)) for p in ...]`: the
            // extractor is handed resolved paths, which is what the log
            // records and what every scanned filename is built from. (The
            // basename the artifacts are named after still comes from the
            // raw argument -- see above.)
            let paths: Vec<String> = args
                .directory_paths
                .iter()
                .map(|p| pathutil::abspath(&p.to_string_lossy()))
                .collect();
            extractor::extract_from_directories(&paths, announce)
        }
    }
}

/// `resolve_test_csv` (AstroBinUpload.py). Two locations, in order:
///
/// 1. Inside `output_dir` -- `<first directory>/AstroBinUploadInfo` -- where
///    a `--debug` run writes `debug_step_00_RawHeaders.csv` and a crash
///    writes `emergency_raw_dump.csv`. A bare filename replays your own
///    debug run.
/// 2. The path exactly as given, resolved from the current directory or as
///    an absolute path -- what every earlier release accepted, and the form
///    that matters for a CSV that came from somewhere else entirely.
///
/// An absolute `given` satisfies both, since `pathutil::join` (like
/// `os.path.join`) discards `output_dir` when `given` is absolute.
///
/// Exits the process with the same three-line diagnostic Python prints when
/// neither candidate exists, rather than returning an error: this mirrors
/// `sys.exit(1)` inside a helper function, which `anyhow::Result` has no
/// clean way to express short of the same hard exit.
fn resolve_test_csv(given: &str, output_dir: &str) -> String {
    let candidates = test_csv_candidates(given, output_dir);
    for candidate in &candidates {
        if std::path::Path::new(candidate).is_file() {
            return candidate.clone();
        }
    }

    println!("\n[ERROR] --test file not found: {given}");
    println!("Looked in both:");
    for candidate in unique_in_order(&candidates) {
        println!("  {}", pathutil::abspath(candidate));
    }
    println!(
        "\nPass either the bare filename of a CSV inside AstroBinUploadInfo, \
         or a path to one elsewhere.\n"
    );
    std::process::exit(1);
}

/// `[os.path.join(output_dir, given), given]` — pure, so the
/// absolute-path collapse (`pathutil::join` discards `output_dir` when
/// `given` is absolute, exactly like `os.path.join`) is testable without
/// the `process::exit` that makes `resolve_test_csv` itself untestable on
/// the not-found path.
fn test_csv_candidates(given: &str, output_dir: &str) -> [String; 2] {
    [pathutil::join(output_dir, given), given.to_string()]
}

/// `dict.fromkeys(candidates)`: first-seen order, duplicates dropped. Used
/// only for the "Looked in both:" listing — an absolute `given` collapses
/// both candidates to the same string, and Python prints it once, not
/// twice.
fn unique_in_order(items: &[String]) -> Vec<&String> {
    let mut seen = Vec::with_capacity(items.len());
    for item in items {
        if !seen.contains(&item) {
            seen.push(item);
        }
    }
    seen
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
    lookup: Option<&crate::sites::SiteLookup>,
    config_path: Option<&std::path::Path>,
) -> Result<Table> {
    // `PipelineProcessor.add_step`, once per registration, before the run.
    for name in STEP_NAMES {
        log_debug!("add_step", 66, "Registered pipeline step: {name}");
    }
    log_info!("run", 83, "Processing state initialized");
    log_info!(
        "run",
        84,
        "Pipeline execution started: {} steps registered.",
        STEP_NAMES.len()
    );

    // Each candidate carries the Python line its `logger.debug` sits on, so
    // the record names the branch `_dump_debug_csv` actually took: the
    // aggregated frame, the processed frame, or -- at step 1 only -- the raw
    // one. When every candidate is empty it writes nothing at all.
    let dump = |i: usize, name: &str, suffix: &str, candidates: &[(&Table, u32)]| {
        let Some(dir) = out_dir else { return };
        let frames: Vec<&Table> = candidates.iter().map(|(t, _)| *t).collect();
        let Some(chosen) = debug_dump_target(&frames) else {
            return;
        };
        let line = candidates
            .iter()
            .find(|(t, _)| std::ptr::eq(*t, chosen))
            .map(|(_, l)| *l)
            .unwrap_or(134);
        let path = pathutil::join(dir, &format!("debug_step_{i:02}_{name}{suffix}.csv"));
        match std::fs::write(&path, exporter::to_csv(chosen)) {
            Ok(()) => match line {
                129 => log_debug!("_dump_debug_csv", 129, "Saved debug state (aggregated) to {path}"),
                139 => log_debug!("_dump_debug_csv", 139, "Saved debug state (raw) to {path}"),
                _ => log_debug!("_dump_debug_csv", 134, "Saved debug state to {path}"),
            },
            // `_dump_debug_csv` swallows its own failures rather than killing
            // a run that was otherwise fine.
            Err(e) => {
                eprintln!("Failed to save debug CSV for {name}: {e}");
                log_error!("_dump_debug_csv", 142, "Failed to save debug CSV for {name}: {e}");
            }
        }
    };

    // The dump of the state a failing step was handed, plus the three records
    // `run`'s except-branch writes.
    let crashed = |i: usize, name: &str, candidates: &[(&Table, u32)], e: &anyhow::Error| {
        if out_dir.is_some() {
            dump(i, name, "_CRASH_DIAGNOSTIC", candidates);
            log_info!("run", 104, "Emergency diagnostic state saved to {}", out_dir.unwrap());
        }
        log_error!("run", 109, "CRITICAL FAILURE in [{name}]: {e}");
        log_error!("run", 112, "{e}");
    };

    // Steps 2-5: one candidate on success (the processed frame), one on
    // failure (the frame the step was handed, which is what `state` still
    // holds).
    macro_rules! step {
        ($i:expr, $name:expr, $input:expr, $call:expr) => {
            match $call {
                Ok(out) => {
                    if debug {
                        dump($i, $name, "", &[(&out, 134)]);
                    }
                    out
                }
                Err(e) => {
                    crashed($i, $name, &[($input, 134)], &e);
                    return Err(e);
                }
            }
        };
    }

    // Step 1 is the only one whose empty result falls back to the raw frame,
    // per `_dump_debug_csv`'s `elif step_index == 1`, and the only one whose
    // crash dump is of the raw frame for the same reason -- `processed_df` is
    // still empty when the first step raises.
    log_debug!("run", 89, "Executing step: {}", STEP_NAMES[0]);
    let df = match steps::normalize::execute(raw, app) {
        Ok(out) => {
            if debug {
                dump(1, "NormalizeHeadersStep", "", &[(&out, 134), (raw, 139)]);
            }
            out
        }
        Err(e) => {
            crashed(1, "NormalizeHeadersStep", &[(raw, 139)], &e);
            return Err(e);
        }
    };
    log_debug!("run", 89, "Executing step: {}", STEP_NAMES[1]);
    let df = step!(2, "OpticalParameterStep", &df, steps::optical::execute(&df, app));
    log_debug!("run", 89, "Executing step: {}", STEP_NAMES[2]);
    let (df, labels) = {
        let input = df;
        match steps::deduplicate::execute_labelled(&input) {
            Ok((out, labels)) => {
                if debug {
                    dump(3, "DeduplicateStep", "", &[(&out, 134)]);
                }
                (out, labels)
            }
            Err(e) => {
                crashed(3, "DeduplicateStep", &[(&input, 134)], &e);
                return Err(e);
            }
        }
    };
    log_debug!("run", 89, "Executing step: {}", STEP_NAMES[3]);
    let df = step!(
        4,
        "CalibrationMatcherStep",
        &df,
        steps::calibration::execute_labelled(&df, &labels)
    );
    log_debug!("run", 89, "Executing step: {}", STEP_NAMES[4]);
    let df = step!(
        5,
        "GeocodeStep",
        &df,
        steps::geocode::execute(&df, app, lookup, config_path)
    );

    // The aggregation step is the one place the dump prefers a different
    // frame: `aggregated_df` when it has rows, falling back to the frame that
    // went in when it does not.
    log_debug!("run", 89, "Executing step: {}", STEP_NAMES[5]);
    match steps::aggregate::execute(&df, app) {
        Ok(agg) => {
            if debug {
                dump(6, "AggregationStep", "", &[(&agg, 129), (&df, 134)]);
            }
            log_info!("run", 117, "Pipeline execution completed successfully.");
            Ok(agg)
        }
        Err(e) => {
            crashed(6, "AggregationStep", &[(&df, 134)], &e);
            Err(e)
        }
    }
}

/// The Python step class names, in registration order.
const STEP_NAMES: [&str; 6] = [
    "NormalizeHeadersStep",
    "OpticalParameterStep",
    "DeduplicateStep",
    "CalibrationMatcherStep",
    "GeocodeStep",
    "AggregationStep",
];

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
    let raw = load_headers(args, false, None)?;
    let app = crate::appconfig::AppConfig::from_config(cfg)?;
    let agg = run_pipeline(&raw, &app, None, false, None, None)?;
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

    let raw = load_headers(args, false, None)?;
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

    let df = steps::geocode::execute(&df, &app, None, None)?;
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

    /// `resolve_test_csv`'s candidate construction (AstroBinUpload.py) --
    /// the part that can be unit-tested without the `process::exit` on its
    /// not-found path.
    ///
    /// The separator is platform-dependent because `pathutil::join` mirrors
    /// `os.path.join`, which is `ntpath.join` on Windows: it appends `\` even
    /// when the left-hand side is spelled with forward slashes. Asserting `/`
    /// unconditionally is what failed the first v2.2.1 release build -- the
    /// port was right and the test was not.
    #[test]
    fn candidates_are_output_dir_join_then_given_as_is() {
        let joined = if cfg!(windows) {
            "/scan/AstroBinUploadInfo\\replay.csv"
        } else {
            "/scan/AstroBinUploadInfo/replay.csv"
        };
        assert_eq!(
            test_csv_candidates("replay.csv", "/scan/AstroBinUploadInfo"),
            [joined.to_string(), "replay.csv".to_string()]
        );
    }

    /// An absolute `given` makes `os.path.join` (and `pathutil::join`)
    /// discard `output_dir` entirely, so both candidates collapse to the
    /// same string. Verified against live Python 2026-09-09: the not-found
    /// diagnostic then prints exactly one path under "Looked in both:",
    /// not two.
    #[test]
    fn an_absolute_given_path_collapses_both_candidates() {
        let candidates = test_csv_candidates("/nope/absent.csv", "/scan/AstroBinUploadInfo");
        assert_eq!(candidates, ["/nope/absent.csv", "/nope/absent.csv"]);
        assert_eq!(unique_in_order(&candidates).len(), 1);
    }

    #[test]
    fn unique_in_order_keeps_first_seen_order_and_drops_repeats() {
        let items = vec!["a".to_string(), "b".to_string(), "a".to_string()];
        let deduped: Vec<&String> = unique_in_order(&items);
        assert_eq!(deduped, vec!["a", "b"]);
    }
}

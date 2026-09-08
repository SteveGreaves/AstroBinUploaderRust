//! `AstroBinUploader.log`, as `initialise_logging` writes it.
//!
//! The Python side configures one `logging.Logger` with a single
//! `FileHandler` and the format
//!
//! ```text
//! %(asctime)s - %(funcName)s - Line: %(lineno)d - %(levelname)s - %(message)s
//! ```
//!
//! `%(funcName)s` and `%(lineno)d` are stdlib logging's own resolution of the
//! *Python* frame that created the record. Nothing in a Rust binary can
//! resolve those, so every call site here passes the Python function name and
//! line number it corresponds to as literals -- see `logging::sites` for the
//! table, and `parity/check_log.py` for the check that keeps them honest.
//! That makes the log diffable against Python's line for line, with only the
//! leading timestamp free to differ; it also means these numbers are a
//! snapshot of the Python source, and the checker is what catches them
//! drifting.
//!
//! The handler is opened in `'w'` mode, so each run truncates the previous
//! log rather than appending to it.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warning,
    Error,
}

impl Level {
    /// `%(levelname)s`.
    fn name(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warning => "WARNING",
            Level::Error => "ERROR",
        }
    }
}

struct Sink {
    file: File,
    /// `logger.setLevel(logging.INFO)`, raised to DEBUG by `--debug`.
    debug: bool,
}

static SINK: OnceLock<Mutex<Option<Sink>>> = OnceLock::new();

fn sink() -> &'static Mutex<Option<Sink>> {
    SINK.get_or_init(|| Mutex::new(None))
}

/// `initialise_logging`. Failure is not fatal on the Python side either --
/// it falls back to a console logger and the run continues -- so a log that
/// cannot be opened leaves every later call a no-op.
pub fn init(path: &Path, debug: bool) {
    match File::create(path) {
        Ok(file) => {
            *sink().lock().unwrap() = Some(Sink { file, debug });
            // The first record the Python logger writes, from inside
            // initialise_logging itself.
            log(
                Level::Info,
                "initialise_logging",
                77,
                "Logging system initialized successfully.",
            );
        }
        Err(e) => {
            eprintln!("Failed to initialise logging at {}: {e}", path.display());
        }
    }
}

/// One `%(asctime)s - ...` record, dropped when the level is below the
/// logger's own.
pub fn log(level: Level, func: &str, line: u32, message: &str) {
    let mut guard = sink().lock().unwrap();
    let Some(s) = guard.as_mut() else { return };
    if level == Level::Debug && !s.debug {
        return;
    }
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    // A write failure here must not take the run down with it: the log is a
    // diagnostic, not an output.
    let _ = writeln!(
        s.file,
        "{ts} - {func} - Line: {line} - {} - {message}",
        level.name()
    );
    let _ = s.file.flush();
}

/// `logger.info(...)` and friends, carrying the Python `funcName`/`lineno`
/// the record is standing in for.
#[macro_export]
macro_rules! log_at {
    ($level:expr, $func:literal, $line:literal, $($arg:tt)*) => {
        $crate::logging::log($level, $func, $line, &format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_info {
    ($func:literal, $line:literal, $($arg:tt)*) => {
        $crate::log_at!($crate::logging::Level::Info, $func, $line, $($arg)*)
    };
}

#[macro_export]
macro_rules! log_debug {
    ($func:literal, $line:literal, $($arg:tt)*) => {
        $crate::log_at!($crate::logging::Level::Debug, $func, $line, $($arg)*)
    };
}

#[macro_export]
macro_rules! log_warning {
    ($func:literal, $line:literal, $($arg:tt)*) => {
        $crate::log_at!($crate::logging::Level::Warning, $func, $line, $($arg)*)
    };
}

#[macro_export]
macro_rules! log_error {
    ($func:literal, $line:literal, $($arg:tt)*) => {
        $crate::log_at!($crate::logging::Level::Error, $func, $line, $($arg)*)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Without `init`, every macro is a no-op rather than a panic -- the
    /// dump paths (`--dump-steps` and friends) never open a log at all.
    #[test]
    fn logging_before_init_is_silently_dropped() {
        log(Level::Info, "main", 158, "no sink yet");
    }

    #[test]
    fn a_record_carries_the_python_function_and_line() {
        let dir = std::env::temp_dir().join(format!("abu-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("AstroBinUploader.log");
        init(&path, false);
        log(Level::Info, "main", 158, "Logging initialized.");
        // DEBUG is below the default level and must not appear.
        log(Level::Debug, "add_step", 66, "Registered pipeline step: X");
        let text = std::fs::read_to_string(&path).unwrap();
        *sink().lock().unwrap() = None;

        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(
            lines[0].ends_with(
                " - initialise_logging - Line: 77 - INFO - Logging system initialized successfully."
            ),
            "{}",
            lines[0]
        );
        assert!(
            lines[1].ends_with(" - main - Line: 158 - INFO - Logging initialized."),
            "{}",
            lines[1]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

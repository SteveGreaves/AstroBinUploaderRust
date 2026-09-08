//! `os.path` — both flavours, the way CPython itself has both.
//!
//! `posixpath` and `ntpath` are plain, always-importable modules on any OS;
//! `os.path` just aliases whichever one matches `os.name` at import time.
//! This file mirrors that on purpose: [`posix`] and [`windows`] are both
//! always compiled and always tested, on any host, and the four top-level
//! functions pick one module by `#[cfg(windows)]` — the same selection
//! `os.path` makes, at compile time instead of import time. That is what
//! lets the Windows-target logic be verified by `cargo test` on this Linux
//! development machine, where it would otherwise never run at all.
//!
//! Rust's `Path` is close enough to be misleading in both directions:
//!
//! - `Path::file_name` answers `Some("b")` for `/a/b/` where Python answers
//!   `""`, and `None` for `.` where Python answers `"."`. That string names
//!   every output file (`basename(argv[1]).replace(" ", "_")`).
//! - `std::fs::canonicalize` resolves symlinks; `os.path.abspath` does not, it
//!   only normalises the string. `SOURCE_PATH` is an `abspath`, and its
//!   `dirname` is half of `DeduplicateStep`'s group key — so under a
//!   symlinked scan path the two would group differently.
//! - Joining is textual, not `PathBuf::join`: the extractor builds
//!   `os.path.join(root, file)` and **sorts those strings**, so the exact
//!   spelling (relative if the CLI argument was relative) is what decides row
//!   order (PORT_PLAN.md hazard 8). `Path` also silently loses information a
//!   non-UTF-8 or drive-relative string carries.
//!
//! Everything here operates on `str`, not `Path`, for that last reason.

#[cfg(windows)]
pub use windows::{abspath, basename, dirname, join};

#[cfg(not(windows))]
pub use posix::{abspath, basename, dirname, join};

/// `posixpath` — Linux and macOS.
pub mod posix {
    /// `posixpath.basename`: everything after the last `/`, and nothing else.
    pub fn basename(p: &str) -> &str {
        match p.rfind('/') {
            Some(i) => &p[i + 1..],
            None => p,
        }
    }

    /// `posixpath.dirname`: everything before the last `/`, with a lone `/`
    /// kept.
    pub fn dirname(p: &str) -> String {
        match p.rfind('/') {
            None => String::new(),
            Some(0) => "/".to_string(),
            Some(i) => p[..i].to_string(),
        }
    }

    /// `posixpath.join(a, b)` for two components: `b` wins outright if
    /// absolute, and a separator is inserted only when `a` does not already
    /// end in one.
    pub fn join(a: &str, b: &str) -> String {
        if b.starts_with('/') {
            return b.to_string();
        }
        if a.is_empty() {
            return b.to_string();
        }
        if a.ends_with('/') {
            format!("{a}{b}")
        } else {
            format!("{a}/{b}")
        }
    }

    /// `posixpath.abspath` = `normpath(join(getcwd(), p))`.
    ///
    /// Purely textual: `..` is resolved lexically and symlinks are left
    /// alone, so this can name a different file than `canonicalize` would.
    /// That is the intended behaviour — it is what the Python writes into
    /// `SOURCE_PATH`.
    pub fn abspath(p: &str) -> String {
        let cwd = std::env::current_dir()
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_default();
        abspath_with_cwd(p, &cwd)
    }

    /// `abspath`'s logic with the current directory supplied rather than
    /// read from the live process — so the join/normalise rules can be
    /// tested against a *known* POSIX-shaped cwd on any host, including one
    /// whose real `current_dir()` is a Windows path (a Windows CI runner,
    /// say). `abspath` above is the thin wrapper that plugs in the real one.
    fn abspath_with_cwd(p: &str, cwd: &str) -> String {
        let joined = if p.starts_with('/') { p.to_string() } else { join(cwd, p) };
        normpath(&joined)
    }

    /// `posixpath.normpath`: collapse repeated separators, drop `.`, resolve
    /// `..` lexically, and never return an empty string.
    ///
    /// The one quirk worth keeping: a path beginning with exactly two
    /// slashes keeps both, because POSIX reserves `//` for the
    /// implementation. Three or more collapse to one.
    pub fn normpath(p: &str) -> String {
        if p.is_empty() {
            return ".".to_string();
        }
        let absolute = p.starts_with('/');
        let leading = if absolute && p.starts_with("//") && !p.starts_with("///") {
            2
        } else if absolute {
            1
        } else {
            0
        };

        let mut out: Vec<&str> = Vec::new();
        for part in p.split('/') {
            match part {
                "" | "." => continue,
                ".." => {
                    // A `..` above the root is dropped; above a relative
                    // path's start it is kept, because there is nothing to
                    // cancel.
                    match out.last() {
                        Some(&last) if last != ".." => {
                            out.pop();
                        }
                        _ if absolute => {}
                        _ => out.push(".."),
                    }
                }
                other => out.push(other),
            }
        }

        let body = out.join("/");
        if leading > 0 {
            format!("{}{body}", "/".repeat(leading))
        } else if body.is_empty() {
            ".".to_string()
        } else {
            body
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Every expectation here is `python3 -c "import posixpath; ..."`.
        #[test]
        fn basename_is_not_path_file_name() {
            assert_eq!(basename("/a/b/c.fits"), "c.fits");
            assert_eq!(basename("/a/b/"), ""); // Path::file_name says Some("b")
            assert_eq!(basename("."), "."); // Path::file_name says None
            assert_eq!(basename("Sadr Region"), "Sadr Region");
            assert_eq!(basename(""), "");
        }

        #[test]
        fn dirname_keeps_a_lone_root_slash() {
            assert_eq!(dirname("/a/b/c.fits"), "/a/b");
            assert_eq!(dirname("c.fits"), "");
            assert_eq!(dirname("/c.fits"), "/");
        }

        #[test]
        fn join_is_textual_and_an_absolute_second_argument_wins() {
            assert_eq!(join("/a/b", "c.fits"), "/a/b/c.fits");
            assert_eq!(join("/a/b/", "c.fits"), "/a/b/c.fits");
            assert_eq!(join("rel", "c.fits"), "rel/c.fits");
            assert_eq!(join("/a", "/b"), "/b");
            assert_eq!(join("", "c.fits"), "c.fits");
        }

        #[test]
        fn normpath_resolves_dot_dot_lexically() {
            assert_eq!(normpath("/a/b/../c"), "/a/c");
            assert_eq!(normpath("/a/./b//c/"), "/a/b/c");
            assert_eq!(normpath("/../.."), "/"); // cannot climb above root
            assert_eq!(normpath("../../a"), "../../a"); // but a relative path can
            assert_eq!(normpath("a/../.."), "..");
            assert_eq!(normpath(""), ".");
            assert_eq!(normpath("."), ".");
            // POSIX reserves exactly two leading slashes; three or more collapse.
            assert_eq!(normpath("//a/b"), "//a/b");
            assert_eq!(normpath("///a/b"), "/a/b");
        }

        #[test]
        fn abspath_leaves_an_absolute_path_alone_apart_from_normalising() {
            assert_eq!(abspath("/a/b/../c"), "/a/c");
        }

        /// `abspath` on a relative path needs a real cwd to prove anything
        /// about, and the *real* `current_dir()` is a POSIX path only on a
        /// POSIX host — a Windows CI runner exposed exactly that when this
        /// test used to call bare `abspath("rel")` and assert the result
        /// started with `/`. Supplying a synthetic cwd tests the join and
        /// normalise rules on every host, this crate's own Linux development
        /// machine included.
        #[test]
        fn abspath_joins_a_relative_path_onto_the_given_cwd() {
            assert_eq!(abspath_with_cwd("rel", "/a/b"), "/a/b/rel");
            assert_eq!(abspath_with_cwd("../rel", "/a/b"), "/a/rel");
        }
    }
}

/// `ntpath` — the Windows targets in Phase 5's release matrix.
///
/// Every rule below is measured (`python3 -c "import ntpath; ..."`), not
/// read off documentation. `ntpath` accepts `/` as an alternate separator on
/// input (`altsep = '/'`) but always normalises to `\`, has drive letters
/// (`C:`) and UNC shares (`\\server\share`) as two different flavours of
/// "absolute", and a `..` collapses lexically the same way `posixpath` does.
///
/// Scoped to what a real Windows invocation of this program can put in
/// `directory_paths`: local drive paths and UNC shares. Device-namespace
/// prefixes (`\\?\`) and 8.3 short names are out of scope — nothing in this
/// pipeline's argument surface produces them.
///
/// `#[allow(dead_code)]`: on every host that actually builds this crate
/// today, `#[cfg(windows)]` above picks [`posix`] instead, so nothing outside
/// `#[cfg(test)]` calls in here — the lint cannot see that `cargo test` still
/// exercises every function through its own test module below. That is the
/// whole reason this module exists as a plain, always-compiled sibling of
/// `posix` rather than a `#[cfg(windows)]`-gated one: to keep it verified by
/// `cargo test` on this Linux development machine, where a gated module
/// would never run at all.
#[allow(dead_code)]
pub mod windows {
    /// `ntpath.splitdrive`: the drive letter (`C:`) or UNC share
    /// (`\\server\share`) at the front of a path, and everything after it.
    /// Neither half is normalised — this is a lexical split, not a semantic
    /// one.
    fn splitdrive(p: &str) -> (&str, &str) {
        let bytes = p.as_bytes();
        if bytes.len() >= 2 && &p[1..2] == ":" {
            return p.split_at(2);
        }
        // `\\server\share\...` — a UNC prefix is two separators, a host
        // component, a separator, then a share component. Anything short of
        // that (a bare `\\`, or `\\server` with no share) has no drive.
        if is_sep(bytes.first()) && is_sep(bytes.get(1)) {
            let rest = &p[2..];
            // `\\server` with no share at all is still a UNC drive on its
            // own — measured: `splitdrive("\\\\server") == ("\\\\server", "")`.
            let Some(host_end) = rest.find(is_sep_char) else {
                return (p, "");
            };
            let after_host = &rest[host_end + 1..];
            let share_end = after_host.find(is_sep_char).unwrap_or(after_host.len());
            let drive_len = 2 + host_end + 1 + share_end;
            return p.split_at(drive_len);
        }
        ("", p)
    }

    fn is_sep_char(c: char) -> bool {
        c == '\\' || c == '/'
    }

    fn is_sep(b: Option<&u8>) -> bool {
        matches!(b, Some(b'\\') | Some(b'/'))
    }

    /// `ntpath.basename`: `split(p)[1]` — everything after the last
    /// separator in the path component (the drive is not part of it).
    pub fn basename(p: &str) -> &str {
        let (_, path) = splitdrive(p);
        match path.rfind(is_sep_char) {
            Some(i) => &path[i + 1..],
            None => path,
        }
    }

    /// `ntpath.dirname`: `split(p)[0]` — the drive plus everything up to the
    /// last separator, with a lone trailing separator kept.
    pub fn dirname(p: &str) -> String {
        let (drive, path) = splitdrive(p);
        match path.rfind(is_sep_char) {
            None => drive.to_string(),
            Some(0) => format!("{drive}{}", &path[..1]),
            Some(i) => format!("{drive}{}", &path[..i]),
        }
    }

    /// `ntpath.isabs`: a UNC drive (`\\server[\share]`) is absolute outright,
    /// with or without a share or trailing path — the host alone anchors it.
    /// A drive letter is absolute only when followed by a separator
    /// (`C:\x`, not the drive-relative `C:x`). A bare leading separator with
    /// no drive at all is absolute too.
    fn isabs(p: &str) -> bool {
        let (drive, rest) = splitdrive(p);
        if drive.starts_with(['\\', '/']) {
            true
        } else {
            !rest.is_empty() && is_sep(rest.as_bytes().first())
        }
    }

    /// `ntpath.join(a, b)`.
    ///
    /// More involved than the POSIX version: an absolute `b` replaces `a`'s
    /// path but keeps `a`'s drive when `b` has none of its own (measured:
    /// `join('C:\\a\\b', '\\c') == 'C:\\c'`), and a drive-only `a` with no
    /// path component gets no separator inserted before a relative `b`
    /// (`join('C:', 'a') == 'C:a'`, drive-relative — distinct from
    /// `'C:\\a'`). Neither of those is reachable from this program's own
    /// argument construction, which only ever joins a directory it just
    /// listed with one of its own entries, but they are cheap to get right
    /// since `splitdrive` already does the hard part.
    pub fn join(a: &str, b: &str) -> String {
        let (a_drive, a_path) = splitdrive(a);
        let (b_drive, b_path) = splitdrive(b);

        if isabs(b) || !b_drive.is_empty() {
            // An absolute (or differently-droved) `b` wins outright, taking
            // `a`'s drive only when `b` supplied none of its own.
            let drive = if b_drive.is_empty() { a_drive } else { b_drive };
            return format!("{drive}{b_path}");
        }
        if a.is_empty() {
            return b.to_string();
        }
        let needs_sep = !a_path.is_empty() && !is_sep(a_path.as_bytes().last());
        if needs_sep {
            format!("{a}\\{b}")
        } else {
            format!("{a}{b}")
        }
    }

    /// `ntpath.abspath` = `normpath(join(getcwd(), p))`, the same shape as
    /// the POSIX version.
    pub fn abspath(p: &str) -> String {
        let cwd = std::env::current_dir()
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_default();
        abspath_with_cwd(p, &cwd)
    }

    /// `abspath`'s logic with the current directory supplied rather than
    /// read from the live process — so it can be tested against a *known*
    /// Windows-shaped cwd on any host, including one whose real
    /// `current_dir()` is a POSIX path (this crate's own Linux development
    /// machine). `abspath` above is the thin wrapper that plugs in the real
    /// one.
    fn abspath_with_cwd(p: &str, cwd: &str) -> String {
        let joined = if isabs(p) { p.to_string() } else { join(cwd, p) };
        normpath(&joined)
    }

    /// `ntpath.normpath`: both separator spellings collapse to `\`,
    /// repeated separators collapse to one, `.` is dropped, `..` resolves
    /// lexically per the same rule `posixpath` uses, and a UNC share's two
    /// leading separators are preserved the way POSIX preserves exactly two.
    pub fn normpath(p: &str) -> String {
        if p.is_empty() {
            return ".".to_string();
        }
        let (drive, path) = splitdrive(p);
        let unc = drive.starts_with(['\\', '/']);
        let absolute = unc || is_sep(path.as_bytes().first());

        let mut out: Vec<&str> = Vec::new();
        for part in path.split(is_sep_char) {
            match part {
                "" | "." => continue,
                ".." => match out.last() {
                    Some(&last) if last != ".." => {
                        out.pop();
                    }
                    _ if absolute => {}
                    _ => out.push(".."),
                },
                other => out.push(other),
            }
        }

        let body = out.join("\\");
        let sep = if absolute { "\\" } else { "" };
        // The drive keeps its own casing and separator spelling; only the
        // path half is rebuilt from `\`-joined components.
        if drive.is_empty() && body.is_empty() {
            ".".to_string()
        } else if drive.is_empty() {
            format!("{sep}{body}")
        } else {
            format!("{drive}{sep}{body}")
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Every expectation here is `python3 -c "import ntpath; ..."`.
        #[test]
        fn basename_and_dirname_strip_the_drive_first() {
            assert_eq!(basename(r"C:\a\b\c.fits"), "c.fits");
            assert_eq!(basename(r"C:\a\b\"), "");
            assert_eq!(basename("."), ".");
            assert_eq!(basename("a/b/c.fits"), "c.fits"); // forward slash accepted
            assert_eq!(basename(r"\\server\share\f.fits"), "f.fits");

            assert_eq!(dirname(r"C:\a\b\c.fits"), r"C:\a\b");
            assert_eq!(dirname("c.fits"), "");
            assert_eq!(dirname(r"C:\c.fits"), r"C:\");
            assert_eq!(dirname("a/b/c.fits"), "a/b");
        }

        #[test]
        fn splitdrive_separates_drive_letters_and_unc_shares() {
            assert_eq!(splitdrive(r"C:\a\b"), ("C:", r"\a\b"));
            assert_eq!(
                splitdrive(r"\\server\share\a"),
                (r"\\server\share", r"\a")
            );
            assert_eq!(splitdrive(r"a\b"), ("", r"a\b"));
            assert_eq!(splitdrive("C:a\\b"), ("C:", "a\\b")); // drive-relative
        }

        #[test]
        fn isabs_needs_a_separator_after_a_drive_letter() {
            assert!(isabs(r"C:\a"));
            assert!(!isabs("C:a")); // drive-relative, not absolute
            assert!(isabs(r"\a")); // rootless-but-driveless is still absolute
            assert!(!isabs("a"));
            // A UNC host alone anchors the path, share or no share.
            assert!(isabs(r"\\server\share"));
            assert!(isabs(r"\\server"));
            assert!(isabs(r"\\server\"));
        }

        #[test]
        fn splitdrive_handles_a_unc_host_with_no_share() {
            // Measured: the whole string is the drive, with an empty tail --
            // not "no drive at all", which the naive read of the loop gives.
            assert_eq!(splitdrive(r"\\server"), (r"\\server", ""));
            assert_eq!(splitdrive(r"\\server\"), (r"\\server\", ""));
        }

        #[test]
        fn join_keeps_the_first_drive_when_the_second_argument_has_none() {
            assert_eq!(join(r"C:\a\b", "c.fits"), r"C:\a\b\c.fits");
            assert_eq!(join(r"C:\a\b\", "c.fits"), r"C:\a\b\c.fits");
            assert_eq!(join("rel", "c.fits"), r"rel\c.fits");
            assert_eq!(join(r"C:\a", r"D:\b"), r"D:\b"); // a differently-drived b wins outright
            assert_eq!(join("", "c.fits"), "c.fits");
            // measured: an absolute-but-driveless b keeps a's drive
            assert_eq!(join(r"C:\a\b", r"\c"), r"C:\c");
        }

        #[test]
        fn normpath_resolves_dot_dot_and_normalises_the_separator() {
            assert_eq!(normpath(r"C:\a\..\c"), r"C:\c");
            assert_eq!(normpath("a/b\\c"), r"a\b\c"); // forward slash normalised away
            assert_eq!(normpath(r"C:\a\.\b"), r"C:\a\b");
            assert_eq!(normpath("."), ".");
            assert_eq!(normpath(""), ".");
            assert_eq!(normpath(r"a\..\.."), "..");
            assert_eq!(normpath(r"\\server\share\..\x"), r"\\server\share\x");
        }

        #[test]
        fn abspath_leaves_an_absolute_path_alone_apart_from_normalising() {
            assert_eq!(abspath(r"C:\a\..\c"), r"C:\c");
        }

        /// The `windows` counterpart of the `posix` test above: a controlled
        /// cwd rather than the real one, so the join/normalise rules are
        /// tested the same way on every host regardless of which OS is
        /// actually running the test.
        #[test]
        fn abspath_joins_a_relative_path_onto_the_given_cwd() {
            assert_eq!(abspath_with_cwd("rel", r"C:\a\b"), r"C:\a\b\rel");
            assert_eq!(abspath_with_cwd(r"..\rel", r"C:\a\b"), r"C:\a\rel");
        }
    }
}

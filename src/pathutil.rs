//! `os.path`, POSIX flavour — the semantics the Python side actually uses.
//!
//! Rust's `Path` is close enough to be misleading. Three of these differ from
//! the obvious `std` call in ways that reach output:
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
//!   order (PORT_PLAN.md hazard 8).
//!
//! Everything here operates on `str`, not `Path`, for that last reason.

/// `os.path.basename`: everything after the last `/`, and nothing else.
pub fn basename(p: &str) -> &str {
    match p.rfind('/') {
        Some(i) => &p[i + 1..],
        None => p,
    }
}

/// `os.path.dirname`: everything before the last `/`, with a lone `/` kept.
pub fn dirname(p: &str) -> String {
    match p.rfind('/') {
        None => String::new(),
        Some(0) => "/".to_string(),
        Some(i) => p[..i].to_string(),
    }
}

/// `os.path.join(a, b)` for two components: `b` wins outright if absolute,
/// and a separator is inserted only when `a` does not already end in one.
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

/// `os.path.abspath` = `normpath(join(getcwd(), p))`.
///
/// Purely textual: `..` is resolved lexically and symlinks are left alone, so
/// this can name a different file than `canonicalize` would. That is the
/// intended behaviour — it is what the Python writes into `SOURCE_PATH`.
pub fn abspath(p: &str) -> String {
    let joined = if p.starts_with('/') {
        p.to_string()
    } else {
        let cwd = std::env::current_dir()
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_default();
        join(&cwd, p)
    };
    normpath(&joined)
}

/// `posixpath.normpath`: collapse repeated separators, drop `.`, resolve `..`
/// lexically, and never return an empty string.
///
/// The one quirk worth keeping: a path beginning with exactly two slashes
/// keeps both, because POSIX reserves `//` for the implementation. Three or
/// more collapse to one.
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
                // A `..` above the root is dropped; above a relative path's
                // start it is kept, because there is nothing to cancel.
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
        assert!(abspath("rel").starts_with('/'));
    }
}

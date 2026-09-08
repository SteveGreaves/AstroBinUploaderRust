//! `DeduplicateStep` — one row per capture, not one per WBPP artefact.
//!
//! PixInsight's WBPP writes `M31_Light_001_c.xisf` next to the
//! `M31_Light_001.fits` it calibrated. Both carry the same exposure, so
//! counting both doubles the integration time. Frames are grouped by
//! `(directory, base capture name)` and one survivor is picked per group.
//!
//! Two details decide the output rather than just the row count:
//!
//! - **The group key is a pair.** Capture tools reuse filenames across
//!   sessions (`Light_0001.fits`), so dropping the directory merges unrelated
//!   frames (A2 upstream). `SOURCE_PATH` is absent only on a `--test` CSV
//!   captured before that fix — `sadr_raw.csv` is exactly such a fixture, and
//!   it is kept that way deliberately to keep the degraded branch covered.
//! - **Groups come out sorted.** `groupby` orders by `(directory, base)`, so
//!   this step reorders every row even when it drops nothing.

use anyhow::Result;

use crate::constants as col;
use crate::steps::astype_str;
use crate::table::Table;

/// Extension preference: PixInsight's own format first, then FITS spellings.
const EXT_PRIORITY: &[(&str, u32)] = &[(".xisf", 0), (".fits", 1), (".fit", 2), (".fts", 3)];

/// WBPP postfix vocabulary. A chain always *starts* with the calibration
/// marker, because WBPP calibrates before any later stage — which is why a
/// bare `_r` or `_b` is not treated as a postfix. Several rigs use
/// single-letter filter names, and `Target_Filter_R.fits` must not be read as
/// a postfixed `Target_Filter.fits`.
const CHAIN_TOKENS: &[&str] = &["cc", "rn", "r", "d", "b", "s", "lps"];
const EXTENSIONS: &[&str] = &[".xisf", ".fits", ".fit", ".fts"];

pub fn execute(table: &Table) -> Result<Table> {
    let mut df = table.clone();
    if df.n_rows == 0 {
        return Ok(df);
    }

    let filenames: Vec<String> = match df.column(col::FILENAME) {
        Some(c) => c.cells.iter().map(astype_str).collect(),
        None => return Ok(df),
    };

    // A row whose filename does not parse has no base name, and pandas drops
    // it: `groupby` discards NaN keys, and the loop skips them explicitly.
    let bases: Vec<Option<String>> = filenames.iter().map(|f| wbpp_base(f)).collect();

    let dirs: Vec<String> = match df.column(col::SOURCE_PATH) {
        Some(c) => c.cells.iter().map(|p| dirname(&astype_str(p))).collect(),
        None => {
            eprintln!(
                "warning: SOURCE_PATH column absent (--test CSV predates A2) -- \
                 deduplicating on filename alone, which can merge \
                 identically-named captures from different directories."
            );
            vec![String::new(); df.n_rows]
        }
    };

    let mut keyed: Vec<((String, String), usize)> = (0..df.n_rows)
        .filter_map(|i| {
            bases[i]
                .clone()
                .map(|b| ((dirs[i].clone(), b), i))
        })
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));

    let mut kept: Vec<usize> = Vec::new();
    let mut g = 0usize;
    while g < keyed.len() {
        let mut end = g + 1;
        while end < keyed.len() && keyed[end].0 == keyed[g].0 {
            end += 1;
        }

        // Prefer the higher-priority extension, then the shorter filename —
        // a raw capture's name is shorter than its processed descendants'.
        // The sort is stable, so a genuine tie is settled by input order,
        // which the extractor makes deterministic (A9 upstream).
        let mut group: Vec<usize> = keyed[g..end].iter().map(|(_, i)| *i).collect();
        group.sort_by_key(|&i| (ext_rank(&filenames[i]), filenames[i].chars().count()));
        kept.push(group[0]);
        g = end;
    }

    if kept.is_empty() {
        // `if final_rows:` — an empty selection leaves the frame alone rather
        // than replacing it with an empty one.
        return Ok(df);
    }

    df = df.take_rows(&kept);
    Ok(df)
}

/// `next((v for k, v in ext_priority.items() if name.lower().endswith(k)), 9)`
fn ext_rank(name: &str) -> u32 {
    let lower = name.to_lowercase();
    EXT_PRIORITY
        .iter()
        .find(|(ext, _)| lower.ends_with(ext))
        .map_or(9, |(_, rank)| *rank)
}

use crate::pathutil::dirname;

/// `RegexPatterns.WBPP_FILENAME`, group 1:
///
/// ```text
/// (.+?)(_(?:c|cc)(?:_(?:cc|rn|r|d|b|s|lps))*)?(\.xisf|\.fits|\.fit|\.fts)$
/// ```
///
/// Transcribed as a search rather than pulled in with a regex engine: the
/// pattern is fixed, and the lazy first group with an end-anchored extension
/// reduces to "the shortest prefix whose remainder is an optional postfix
/// chain followed by an extension". Matching is case-insensitive
/// (`flags=re.IGNORECASE`).
///
/// The anchoring matters. The pattern this replaced was unanchored with a
/// `_c.*` postfix, so a `_c` anywhere — inside `_calibrated_`, say — swallowed
/// everything up to the extension and silently merged unrelated captures (A1
/// upstream).
fn wbpp_base(name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    // `.+?` is lazy and needs at least one character, so try each split point
    // left to right and take the first that works — the same order the regex
    // engine tries them in.
    for k in 1..lower.len() {
        if !lower.is_char_boundary(k) {
            continue;
        }
        if matches_tail(&lower[k..]) {
            return Some(name[..k].to_string());
        }
    }
    None
}

/// `(_(?:c|cc)(?:_(?:cc|rn|r|d|b|s|lps))*)?(\.xisf|\.fits|\.fit|\.fts)$`
fn matches_tail(rest: &str) -> bool {
    if is_extension(rest) {
        return true;
    }
    for marker in ["_c", "_cc"] {
        if let Some(after) = rest.strip_prefix(marker) {
            if matches_chain(after) {
                return true;
            }
        }
    }
    false
}

/// `(?:_(?:cc|rn|r|d|b|s|lps))*` followed by the extension.
fn matches_chain(rest: &str) -> bool {
    if is_extension(rest) {
        return true;
    }
    for token in CHAIN_TOKENS {
        let prefixed = format!("_{token}");
        if let Some(after) = rest.strip_prefix(prefixed.as_str()) {
            if matches_chain(after) {
                return true;
            }
        }
    }
    false
}

fn is_extension(rest: &str) -> bool {
    EXTENSIONS.contains(&rest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::Cell;

    #[test]
    fn a_postfix_chain_is_stripped_but_a_bare_token_is_not() {
        assert_eq!(wbpp_base("M31_Light_001.fits").as_deref(), Some("M31_Light_001"));
        assert_eq!(wbpp_base("M31_Light_001_c.xisf").as_deref(), Some("M31_Light_001"));
        assert_eq!(
            wbpp_base("M31_Light_001_c_cc_r.xisf").as_deref(),
            Some("M31_Light_001")
        );
        // A1: '_c' inside a longer word must not start a postfix chain.
        assert_eq!(
            wbpp_base("M31_calibrated_001.fits").as_deref(),
            Some("M31_calibrated_001")
        );
        // A single-letter filter name is not a postfix.
        assert_eq!(wbpp_base("Target_Filter_R.fits").as_deref(), Some("Target_Filter_R"));
        // No recognised extension at all: no base name, and the row is
        // dropped.
        assert_eq!(wbpp_base("notes.txt"), None);
    }

    #[test]
    fn matching_is_case_insensitive_but_the_base_keeps_its_case() {
        assert_eq!(wbpp_base("M31_Light_001_C.FITS").as_deref(), Some("M31_Light_001"));
    }

    #[test]
    fn xisf_wins_over_fits_and_the_shorter_name_wins_a_tie() {
        let df = Table::parse_str(
            "filename,n\n\
             M31_001_c.xisf,1\n\
             M31_001.fits,2\n",
        )
        .unwrap();
        let out = execute(&df).unwrap();
        assert_eq!(out.n_rows, 1);
        assert_eq!(out.column("n").unwrap().cells[0], Cell::Int(1));
    }

    #[test]
    fn identical_names_in_different_directories_stay_separate() {
        let df = Table::parse_str(
            "filename,source_path,n\n\
             Light_0001.fits,/data/a/Light_0001.fits,1\n\
             Light_0001.fits,/data/b/Light_0001.fits,2\n",
        )
        .unwrap();
        let out = execute(&df).unwrap();
        assert_eq!(out.n_rows, 2);
    }

    #[test]
    fn without_source_path_the_same_names_collapse() {
        let df = Table::parse_str(
            "filename,n\n\
             Light_0001.fits,1\n\
             Light_0001.fits,2\n",
        )
        .unwrap();
        assert_eq!(execute(&df).unwrap().n_rows, 1);
    }

    #[test]
    fn groups_come_out_sorted_by_directory_then_base_name() {
        let df = Table::parse_str(
            "filename,n\n\
             z.fits,1\n\
             a.fits,2\n",
        )
        .unwrap();
        let out = execute(&df).unwrap();
        assert_eq!(
            out.column("n").unwrap().cells,
            vec![Cell::Int(2), Cell::Int(1)]
        );
    }

    #[test]
    fn dirname_matches_os_path_dirname() {
        assert_eq!(dirname("/a/b/c.fits"), "/a/b");
        assert_eq!(dirname("c.fits"), "");
        assert_eq!(dirname("/c.fits"), "/");
    }
}

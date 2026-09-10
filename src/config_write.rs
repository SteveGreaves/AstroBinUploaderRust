//! Writes `config.ini` — the two operations Python's `ConfigObj` performs on
//! disk: generating a fresh default file, and appending one resolved
//! `[[site]]` block to an existing one (`engine/loader.py`'s
//! `_generate_default_config` and `engine/sites.py`'s `SiteLookup.save`).
//!
//! ## Why one module serves both
//!
//! Per `PORT_PLAN.md` Phase 7's structural insight: the port reads ini but
//! cannot write it, and both gaps need that one missing capability. But the
//! two operations turned out to need genuinely different techniques, not a
//! single one — measured, not assumed, before writing either:
//!
//! - **Generation** builds a `ConfigObj` purely in memory (`config[SECTION]
//!   = {...}` assignments) — never parsed from text — so `indent_type` is
//!   never set from a parsed line and defaults to `''`: the emitted file
//!   carries **no indentation at all**, unlike the hand-formatted
//!   `config.ini.example` this repo ships (8 spaces/level).
//!   [`GENERATED_DEFAULT_CONFIG`] is that exact output, captured from a real
//!   run rather than assembled from the source's literal dicts — this
//!   project's own recorded lesson is to copy an oracle's output, not
//!   transcribe it.
//! - **Site write-back** re-parses the user's *existing* file with
//!   `ConfigObj`, adds one key under `[sites]`, and calls `write()`.
//!   Measured (`/tmp/.../rt` round-trip probe, 2026-09-09): that round-trip
//!   is byte-faithful on every config this project has on disk — the golden
//!   harness fixture, `config.ini.example`, and the maintainer's
//!   own `config.ini`. So the port does not need a general ini writer, only
//!   a splice: locate `[sites]`, leave every other byte untouched, and
//!   insert one indented block exactly where Python's dict-ordered write
//!   would put it (right after the section's last existing entry, or
//!   immediately after the header when the section is empty or newly
//!   created — both measured against live `configobj`, not inferred from
//!   its docs).
//!
//! Bounded deliberately: [`save_site`] only ever inserts under `[sites]`,
//! never edits an existing line. It does not attempt general-purpose
//! round-trip preservation (line continuations, `unrepr` mode, multi-line
//! values) — nothing in this program's config model produces those, so
//! `configobj` never emits them into a file this splice will later read.

use crate::config::parse_header;
use crate::numeric::python_repr_f64;
use anyhow::{Context, Result};
use std::path::Path;

/// Byte-for-byte the file `ConfigLoader._generate_default_config` writes,
/// captured from a real run of Python v2.2.0:
///
/// ```text
/// PYTHONPATH=../AstroBinUploader python3 -c "
/// import logging
/// from engine.loader import ConfigLoader
/// logger = logging.getLogger('gen'); logger.addHandler(logging.NullHandler())
/// try:
///     ConfigLoader(logger).load('config.ini')
/// except SystemExit:
///     pass
/// "
/// ```
///
/// Deterministic — run twice, byte-identical both times (verified before
/// this was captured). 103 lines, 3111 bytes, ends `[sites]\n` with no
/// trailing blank line.
pub const GENERATED_DEFAULT_CONFIG: &str = r#"# AstroBinUpload configuration.
#
# Generated automatically on first run. Edit to match your own
# equipment and site, then run the script again with your data
# directory. Keep a backup once it is personalised.
#
# Full documentation of every section is in the README.

# Values used when a header is missing, or when a frame leaves the
# field blank. These are what a frame falls back to, so set them to
# your own equipment and site rather than leaving the placeholders.
#
# USEOBSDATE = True  aggregate by each frame's calendar date.
#             False  frames taken after midnight count with the
#                    session that started the previous evening.
[defaults]
IMAGETYP = LIGHT
EXPOSURE = 0.0
DATE-OBS = 2023-01-01
XBINNING = 1
GAIN = -1
EGAIN = -1
INSTRUME = None
TELESCOP = None
FOCNAME = None
FWHEEL = None
ROTNAME = None
ROTANTANG = 0
XPIXSZ = 3.76
CCD-TEMP = -10
FOCALLEN = 500
FOCRATIO = 5.0
SITE = Unknown Site
SITELAT = 0.0
SITELONG = 0.0
BORTLE = 4
SQM = 21.0
FILTER = No Filter
OBJECT = No target
FOCTEMP = 20
HFR = 1.6
SWCREATE = Unknown package
USEOBSDATE = True

# Read a non-standard header keyword as a standard one:
#   STANDARD_NAME = YOUR_KEYWORD
# A comma-separated list tries each keyword in turn, which is how
# one config covers several capture packages.
[override]
SITE = SITENAME
EXPOSURE = EXPTIME
INSTRUME = CAMERA_MODEL
FOCNAME = FOCUSER
SWCREATE = CREATOR
SQM = "AOCSKYQ, AOCSKYQU"
FOCTEMP = AOCAMBT

# Force a display value for equipment fields when the header text
# is unhelpful -- N.I.N.A. writes "EAF" where AstroBin expects
# "ZWO EAF", for example. "None" leaves the value as found.
[equipmentoverrides]
INSTRUME = None
TELESCOP = None
FOCNAME = None
FWHEEL = None
ROTNAME = None

# Filter name -> AstroBin filter ID.
# These defaults are the author's own Astronomik 2 inch round
# filters, named as N.I.N.A. writes them. An AstroBin ID identifies
# a specific filter product, so the same name in a different brand,
# size or mounting has a different ID -- replace these with your own.
# See "Finding AstroBin's Numeric ID for Filters" in the README.
[filters]
Ha = 4663
SII = 4844
OIII = 4752
Red = 4649
Green = 4643
Blue = 4637
Lum = 2906

# Sky quality API key and endpoint, plus your e-mail address.
# Only the API key is to be edited: replace YOUR_API_KEY with the
# key itself. The key is obtained by e-mailing the owner of
# lightpollutionmap.info -- see the README.
#
# EMAIL_ADDRESS is sent to the reverse-geocoding provider as a
# courtesy so they can see who is using their API, and must be a
# real address.
#
# Until a valid key is set, Bortle and SQM come from [defaults]
# BORTLE and SQM; if the address lookup fails, the site details
# come from [defaults] SITE, SITELAT and SITELONG. The run always
# completes either way.
[secret]
YOUR_API_KEY = https://www.lightpollutionmap.info/QueryRaster/
EMAIL_ADDRESS = your_email@example.com

# Written by the program as each new site is resolved, so a site is
# looked up once and never again. You do not normally edit this,
# but a remote site can be added by hand if the lookup cannot run.
[sites]
"#;

/// Writes a fresh default `config.ini` at `path`.
///
/// Mirrors `ConfigLoader.load`'s no-config branch. The caller is
/// responsible for the "created; please edit and re-run" message and
/// `exit(0)` — wired in `main.rs`'s Step-0 bootstrap and `load_config`
/// (Phase 7C).
pub fn write_default_config(path: &Path) -> Result<()> {
    std::fs::write(path, GENERATED_DEFAULT_CONFIG)
        .with_context(|| format!("writing generated config to {}", path.display()))
}

/// A resolved site, ready to append to `[sites]`. Mirrors the keyword
/// arguments to `SiteLookup.save` in `engine/sites.py`.
pub struct ResolvedSite<'a> {
    pub name: &'a str,
    pub latitude: f64,
    pub longitude: f64,
    pub bortle: i64,
    pub sqm: f64,
}

/// Appends `site` to the config file at `path`, under `[sites]` —
/// preserving every other byte, matching `SiteLookup.save`'s round-trip.
///
/// This function starts from `site` already resolved to a real name; it
/// does not repeat Python's earlier guard `if not site or site ==
/// self.config.defaults.get(FITSKeywords.SITE): return False`, which
/// refuses to write the `[defaults] SITE` fallback name back into `[sites]`
/// as if it had been resolved. That check belongs to the caller —
/// `SiteLookup.save`'s port, Phase 7E — before `save_site` is ever reached.
///
/// Returns `Ok(false)` without touching the file when `site.name` already
/// has an entry under `[sites]` (case-sensitive, matching Python's `if site
/// in cfg[section_name]`), exactly as Python's save silently no-ops rather
/// than overwriting a resolved site. The caller — `SiteLookup.save`'s port,
/// Phase 7E — decides what a false return means; this function only
/// guarantees it never overwrote or reordered anything.
///
/// `#[allow(dead_code)]`: not called yet. `SiteLookup`'s port (Phase 7E) is
/// the only caller — `splice_site` underneath is exercised directly by this
/// module's own tests in the meantime.
#[allow(dead_code)]
pub fn save_site(path: &Path, site: &ResolvedSite) -> Result<bool> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading config file {}", path.display()))?;
    let spliced = splice_site(&text, site)?;
    match spliced {
        None => Ok(false),
        Some(new_text) => {
            std::fs::write(path, new_text)
                .with_context(|| format!("writing config file {}", path.display()))?;
            Ok(true)
        }
    }
}

/// The splice itself, pure and testable without touching disk. `None` means
/// "site already present, nothing to do."
fn splice_site(text: &str, site: &ResolvedSite) -> Result<Option<String>> {
    // One pass over physical lines, tracking (a) the section-name path at
    // each depth, exactly as config.rs's parser does, and (b) the file's
    // indent unit -- the literal leading whitespace of the first *header or
    // key=value* line encountered, whatever its depth. Read from configobj
    // source directly (`_load`, around `if indent and self.indent_type is
    // None: self.indent_type = indent`): a comment or blank line is handled
    // in an earlier branch that `continue`s before that check runs, so it
    // never sets indent_type even if it is indented differently from the
    // keys around it. Reproduced faithfully, comment lines excluded, rather
    // than assumed from the files this project happens to have on disk.
    let mut indent_unit: Option<&str> = None;
    // The file's own line ending, detected from its first terminated line.
    // Measured against live configobj (2026-09-09): a CRLF file keeps CRLF
    // on every existing line *and* on the newly appended block, so a config
    // edited on Windows -- one of five release targets -- must not get LF
    // stitched into it.
    let mut line_ending: Option<&str> = None;
    let mut path: Vec<String> = Vec::new();
    // Byte offset of the line *after* the last line that is inside
    // `[sites]` (i.e. the insertion point), and whether `[sites]` was seen
    // at all. `sites_end` is updated every time a line inside the section
    // is confirmed, so after the loop it points just past the section's
    // last line.
    let mut sites_seen = false;
    let mut insert_at: usize = text.len(); // default: end of file
    let mut existing_names: Vec<String> = Vec::new();

    let mut offset = 0usize;
    for raw in text.split_inclusive('\n') {
        offset += raw.len();

        if line_ending.is_none() && raw.ends_with('\n') {
            line_ending = Some(if raw.ends_with("\r\n") { "\r\n" } else { "\n" });
        }
        let trimmed = raw.trim_end_matches(['\n', '\r']);
        let content = trimmed.trim_start();
        let leading_ws = &trimmed[..trimmed.len() - content.len()];

        if content.is_empty() || content.starts_with('#') || content.starts_with(';') {
            // Blank/comment lines do NOT extend the section boundary --
            // measured against live configobj: a trailing blank line (or
            // comment) between [sites]'s last entry and the next section
            // header stays attached to *that following section*, not to
            // [sites]. So the insertion point is left exactly where the
            // last real content line ended, and the new block lands before
            // the blank/comment run rather than after it. Nor do they set
            // indent_unit -- see the comment above the loop.
            continue;
        }
        if !leading_ws.is_empty() && indent_unit.is_none() {
            indent_unit = Some(leading_ws);
        }

        if content.starts_with('[') {
            let (name, depth) = parse_header(content)
                .with_context(|| format!("bad section header {raw:?}"))?;
            if depth == 1 && name.eq_ignore_ascii_case("sites") {
                sites_seen = true;
                path = vec![name];
                insert_at = offset; // empty section: insert right after the header
                continue;
            }
            if depth == 1 {
                // Any other top-level header ends [sites] if we were in it;
                // insert_at was already left at the end of the last line
                // that belonged to the section.
                path = vec![name];
                continue;
            }
            // depth >= 2: nested inside whatever depth == 1 section is open.
            path.truncate(depth - 1);
            path.push(name.clone());
            if in_sites(&path) && depth == 2 {
                existing_names.push(name);
            }
            if in_sites(&path) {
                insert_at = offset;
            }
            continue;
        }

        // A `key = value` line.
        if in_sites(&path) {
            insert_at = offset;
        }
    }

    if sites_seen && existing_names.iter().any(|n| n == site.name) {
        return Ok(None);
    }

    let indent = indent_unit.unwrap_or("");
    let nl = line_ending.unwrap_or("\n");
    let block = format_site_block(site, indent, nl);

    let mut out = String::with_capacity(text.len() + block.len() + 16);
    out.push_str(&text[..insert_at]);
    // Ensure the chunk just copied ends in a newline before anything is
    // appended after it. Measured against live configobj: even an
    // *unmutated* round-trip through `ConfigObj(...).write()` adds a
    // trailing newline to a file that lacked one -- `write()` never leaves
    // a bare EOF -- so this applies whenever `insert_at` reaches end of
    // file, not only in the no-`[sites]`-section case. Mid-file, `insert_at`
    // always already follows a newline by construction, so this is a no-op
    // there.
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(nl);
    }
    if !sites_seen {
        // No [sites] section at all: create it at end of file, matching
        // `cfg[section_name] = {}` then the same append path.
        out.push_str("[sites]");
        out.push_str(nl);
    }
    // When the section already existed, its header line was already copied
    // verbatim via `text[..insert_at]`, case and all -- nothing to re-emit.
    out.push_str(&block);
    out.push_str(&text[insert_at..]);

    Ok(Some(out))
}

fn in_sites(path: &[String]) -> bool {
    path.first()
        .is_some_and(|s| s.eq_ignore_ascii_case("sites"))
}

fn format_site_block(site: &ResolvedSite, indent: &str, nl: &str) -> String {
    format!(
        "{i1}[[{name}]]{nl}{i2}latitude = {lat}{nl}{i2}longitude = {lon}{nl}{i2}bortle = {bortle}{nl}{i2}sqm = {sqm}{nl}",
        i1 = indent,
        i2 = indent.repeat(2),
        name = quote_if_needed(site.name),
        lat = python_repr_f64(site.latitude),
        lon = python_repr_f64(site.longitude),
        bortle = site.bortle,
        sqm = python_repr_f64(site.sqm),
    )
}

/// Wraps `s` in double quotes if `configobj`'s writer would need to, so it
/// re-parses as the same scalar/name rather than a list or something
/// mis-split on a comment marker.
///
/// Measured against live `configobj` 5.0.8 (2026-09-09): quoting triggers
/// on an empty string, a comma, a `#` anywhere, or leading/trailing
/// whitespace — nothing else (internal `'` / `"` alone do not trigger it).
/// Out of scope, and unreachable for every value this module ever writes
/// (site names, numbers): a value containing *both* quote characters, which
/// needs escaping `configobj` doesn't do trivially either.
fn quote_if_needed(s: &str) -> String {
    let needs = s.is_empty()
        || s.contains(',')
        || s.contains('#')
        || s != s.trim();
    if !needs {
        return s.to_string();
    }
    if s.contains('"') {
        format!("'{s}'")
    } else {
        format!("\"{s}\"")
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_repr_matches_python_for_every_value_this_module_writes() {
        // Verified against live Python `repr()` on 2026-09-09.
        assert_eq!(python_repr_f64(52.2484), "52.2484");
        assert_eq!(python_repr_f64(-0.1231), "-0.1231");
        assert_eq!(python_repr_f64(40.75), "40.75");
        assert_eq!(python_repr_f64(-111.8833), "-111.8833");
        assert_eq!(python_repr_f64(21.0), "21.0");
        assert_eq!(python_repr_f64(1.0), "1.0");
        assert_eq!(python_repr_f64(0.0), "0.0");
        assert_eq!(python_repr_f64(20.5), "20.5");
    }

    #[test]
    fn quoting_matches_configobj_rules() {
        assert_eq!(quote_if_needed("Papworth Everard"), "Papworth Everard");
        assert_eq!(
            quote_if_needed("Norton Close, Papworth Everard"),
            "\"Norton Close, Papworth Everard\""
        );
        assert_eq!(quote_if_needed(""), "\"\"");
        assert_eq!(quote_if_needed("has#hash"), "\"has#hash\"");
        assert_eq!(quote_if_needed(" leading"), "\" leading\"");
    }

    #[test]
    fn appends_to_an_existing_empty_sites_section() {
        let input = "[defaults]\n        SITE = X\n[sites]\n[filters]\n        Ha = 4663\n";
        let site = ResolvedSite {
            name: "New Site",
            latitude: 1.0,
            longitude: 2.0,
            bortle: 4,
            sqm: 21.0,
        };
        let out = splice_site(input, &site).unwrap().unwrap();
        assert_eq!(
            out,
            "[defaults]\n        SITE = X\n[sites]\n        [[New Site]]\n                latitude = 1.0\n                longitude = 2.0\n                bortle = 4\n                sqm = 21.0\n[filters]\n        Ha = 4663\n"
        );
    }

    #[test]
    fn appends_after_the_last_existing_site_before_the_next_top_level_section() {
        let input = "[sites]\n        [[Site A]]\n                latitude = 1.0\n                longitude = 2.0\n                bortle = 4\n                sqm = 21\n[filters]\n        Ha = 4663\n";
        let site = ResolvedSite {
            name: "Site B",
            latitude: 3.0,
            longitude: 4.0,
            bortle: 5,
            sqm: 20.0,
        };
        let out = splice_site(input, &site).unwrap().unwrap();
        assert_eq!(
            out,
            "[sites]\n        [[Site A]]\n                latitude = 1.0\n                longitude = 2.0\n                bortle = 4\n                sqm = 21\n        [[Site B]]\n                latitude = 3.0\n                longitude = 4.0\n                bortle = 5\n                sqm = 20.0\n[filters]\n        Ha = 4663\n"
        );
    }

    #[test]
    fn creates_the_section_at_end_of_file_when_absent() {
        let input = "[defaults]\n        SITE = X\n[filters]\n        Ha = 4663\n";
        let site = ResolvedSite {
            name: "New Site",
            latitude: 1.0,
            longitude: 2.0,
            bortle: 4,
            sqm: 21.0,
        };
        let out = splice_site(input, &site).unwrap().unwrap();
        assert_eq!(
            out,
            "[defaults]\n        SITE = X\n[filters]\n        Ha = 4663\n[sites]\n        [[New Site]]\n                latitude = 1.0\n                longitude = 2.0\n                bortle = 4\n                sqm = 21.0\n"
        );
    }

    #[test]
    fn no_indentation_at_all_when_the_file_has_none() {
        // The port's own zero-indent generated file, per GENERATED_DEFAULT_CONFIG.
        let input = "[defaults]\nSITE = X\n[sites]\n";
        let site = ResolvedSite {
            name: "New Site",
            latitude: 1.0,
            longitude: 2.0,
            bortle: 4,
            sqm: 21.0,
        };
        let out = splice_site(input, &site).unwrap().unwrap();
        assert_eq!(
            out,
            "[defaults]\nSITE = X\n[sites]\n[[New Site]]\nlatitude = 1.0\nlongitude = 2.0\nbortle = 4\nsqm = 21.0\n"
        );
    }

    #[test]
    fn existing_site_is_a_no_op() {
        let input = "[sites]\n        [[Papworth Everard]]\n                latitude = 52.2484\n                longitude = -0.1231\n                bortle = 4\n                sqm = 21\n";
        let site = ResolvedSite {
            name: "Papworth Everard",
            latitude: 99.0,
            longitude: 99.0,
            bortle: 9,
            sqm: 9.0,
        };
        assert!(splice_site(input, &site).unwrap().is_none());
    }

    #[test]
    fn a_blank_line_before_the_next_section_stays_attached_to_that_section() {
        // Measured against live configobj 5.0.8: the blank separator line
        // is *not* part of [sites]'s content, so a new site is inserted
        // before it, not after.
        let input = "[sites]\n        [[Site A]]\n                latitude = 1.0\n                longitude = 2.0\n                bortle = 4\n                sqm = 21\n\n[filters]\n        Ha = 4663\n";
        let site = ResolvedSite {
            name: "Site B",
            latitude: 3.0,
            longitude: 4.0,
            bortle: 5,
            sqm: 20.0,
        };
        let out = splice_site(input, &site).unwrap().unwrap();
        assert_eq!(
            out,
            "[sites]\n        [[Site A]]\n                latitude = 1.0\n                longitude = 2.0\n                bortle = 4\n                sqm = 21\n        [[Site B]]\n                latitude = 3.0\n                longitude = 4.0\n                bortle = 5\n                sqm = 20.0\n\n[filters]\n        Ha = 4663\n"
        );
    }

    #[test]
    fn a_comma_bearing_site_name_is_quoted() {
        let input = "[sites]\n";
        let site = ResolvedSite {
            name: "Norton Close, Papworth Everard",
            latitude: 52.2484,
            longitude: -0.1231,
            bortle: 4,
            sqm: 21.0,
        };
        let out = splice_site(input, &site).unwrap().unwrap();
        assert!(out.contains("[[\"Norton Close, Papworth Everard\"]]"));
    }

    #[test]
    fn a_file_missing_its_final_newline_gets_one_before_the_new_block() {
        // Measured against live configobj: even an *unmutated* round-trip
        // through `ConfigObj(...).write()` adds the missing trailing
        // newline -- `write()` never leaves a bare EOF. Without the fix,
        // the new block glued directly onto the last existing line
        // (`sqm = 21        [["New Site"]]`), which config.ini.example
        // -- the file `cp`'d to a new user's config.ini -- would have hit,
        // since it ships with no trailing newline and [sites] is last.
        let input = "[sites]\n        [[Old]]\n                latitude = 1\n                longitude = 2\n                bortle = 4\n                sqm = 21";
        let site = ResolvedSite {
            name: "New Site",
            latitude: 1.0,
            longitude: 2.0,
            bortle: 4,
            sqm: 21.0,
        };
        let out = splice_site(input, &site).unwrap().unwrap();
        assert_eq!(
            out,
            "[sites]\n        [[Old]]\n                latitude = 1\n                longitude = 2\n                bortle = 4\n                sqm = 21\n        [[New Site]]\n                latitude = 1.0\n                longitude = 2.0\n                bortle = 4\n                sqm = 21.0\n"
        );
    }

    #[test]
    fn a_crlf_file_keeps_crlf_on_the_new_block_too() {
        // Measured against live configobj: a file using \r\n keeps \r\n on
        // every existing line *and* the newly appended block uses \r\n as
        // well -- not just preserved, but matched. One of five release
        // targets is Windows, where a Notepad-edited config.ini is CRLF.
        let input = "[sites]\r\n        [[Old]]\r\n                latitude = 1\r\n                longitude = 2\r\n                bortle = 4\r\n                sqm = 21\r\n";
        let site = ResolvedSite {
            name: "New Site",
            latitude: 1.0,
            longitude: 2.0,
            bortle: 4,
            sqm: 21.0,
        };
        let out = splice_site(input, &site).unwrap().unwrap();
        assert_eq!(
            out,
            "[sites]\r\n        [[Old]]\r\n                latitude = 1\r\n                longitude = 2\r\n                bortle = 4\r\n                sqm = 21\r\n        [[New Site]]\r\n                latitude = 1.0\r\n                longitude = 2.0\r\n                bortle = 4\r\n                sqm = 21.0\r\n"
        );
    }

    #[test]
    fn a_comment_line_more_indented_than_keys_does_not_skew_the_indent_unit() {
        // Measured against configobj source directly (`_load`): a comment
        // or blank line `continue`s before the indent_type check runs, so
        // it never sets indent_type even if it is indented differently
        // from the keys around it.
        let input = "[sites]\n                # deeper than the keys\n        [[Old]]\n                latitude = 1\n                longitude = 2\n                bortle = 4\n                sqm = 21\n";
        let site = ResolvedSite {
            name: "New Site",
            latitude: 1.0,
            longitude: 2.0,
            bortle: 4,
            sqm: 21.0,
        };
        let out = splice_site(input, &site).unwrap().unwrap();
        assert!(out.contains("\n        [[New Site]]\n                latitude"));
    }

    #[test]
    fn generated_default_config_ends_with_bare_sites_header() {
        assert!(GENERATED_DEFAULT_CONFIG.ends_with("[sites]\n"));
        assert!(!GENERATED_DEFAULT_CONFIG.ends_with("[sites]\n\n"));
    }

    /// Pins `GENERATED_DEFAULT_CONFIG` against a real capture of Python
    /// v2.2.0's own generator output, committed at
    /// `parity/references/generated_config_v2.2.0.ini` (reproduction
    /// command in that constant's doc comment). Without this, the constant
    /// is only ever checked against *itself* by the tests below it -- a
    /// transcription could drift from the actual oracle with nothing to
    /// catch it, exactly the failure Phase 7A1 had to clean up for the log
    /// literals. When upstream's generator changes, re-run the capture and
    /// update both this fixture and the constant together.
    #[test]
    fn generated_default_config_matches_the_committed_python_oracle() {
        let oracle = include_str!("../parity/references/generated_config_v2.2.0.ini");
        assert_eq!(GENERATED_DEFAULT_CONFIG, oracle);
    }

    #[test]
    fn write_default_config_writes_the_literal_bytes() {
        let dir = tempdir();
        let path = dir.join("config.ini");
        write_default_config(&path).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, GENERATED_DEFAULT_CONFIG);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tempdir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "astrobin_config_write_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

//! `_read_xisf` — PixInsight's XML header block.
//!
//! An XISF file opens with `XISF0100`, a little-endian `u32` giving the XML
//! block's length, four reserved bytes, then that many bytes of UTF-8. The
//! image data follows and is never touched, which is why a 734 MB master
//! truncates to a 190 KiB fixture.
//!
//! Everything a `FITSKeyword` element carries is an XML **attribute**, so
//! every value this reader produces is a string — the whole raw frame from an
//! XISF scan is `object` dtype apart from `NUMBER`. That is the one real
//! simplification over the FITS side.
//!
//! Four fallbacks sit on top, in the order the Python applies them, and each
//! only fires when the one before left the field missing:
//!
//! 1. `instrument:gain` property, with the "a value below 1.0 is really an
//!    EGAIN" heuristic;
//! 2. `GAIN[_-]?(\d+)` out of the filename;
//! 3. `FILTER[_-]([^_.]+)` out of the filename;
//! 4. `NUMBER` from the `PixInsight:ProcessingHistory` property's nested XML,
//!    then from a `numberOfImages` comment on a `COMMENT`/`HISTORY` keyword.

use anyhow::{bail, Context, Result};

use crate::extractor::{HeaderValue, Record};
use crate::pathutil;

const NS: &str = "http://www.pixinsight.com/xisf";

/// Values the Python treats as "not really set" before trying a fallback:
/// `str(v).strip() in ['', 'nan', 'None']`.
fn is_unset(hdr: &Record, key: &str) -> bool {
    match hdr.get(key) {
        None => true,
        Some(HeaderValue::Str(s)) => matches!(s.trim(), "" | "nan" | "None"),
        Some(_) => false,
    }
}

pub fn read_xisf(path: &str) -> Result<Record> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {path}"))?;
    if bytes.len() < 16 {
        bail!("file is too short to hold an XISF header");
    }
    // The Python skips the signature without checking it; a file that is not
    // XISF simply fails to parse as XML a moment later.
    let length = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    let end = 16usize.saturating_add(length).min(bytes.len());
    // `.decode('utf-8', errors='ignore')` — invalid sequences are dropped.
    let xml = String::from_utf8_lossy(&bytes[16..end]).replace('\u{FFFD}', "");

    let doc = roxmltree::Document::parse(&xml).context("parsing the XISF XML header")?;
    let root = doc.root_element();

    // `{kw.get('name'): kw.get('value') for kw in root.findall('.//xisf:FITSKeyword')}`
    // -- a dict comprehension, so a repeated keyword keeps its first position
    // and its *last* value. COMMENT and HISTORY repeat constantly in
    // PixInsight output and collapse to one cell here, unlike the FITS side
    // where they accumulate. The repeated text is still reachable: the
    // NUMBER fallback below re-walks the elements rather than reading them
    // back out of the dict.
    let mut hdr = Record::default();
    for kw in root.descendants().filter(|n| is_xisf(n, "FITSKeyword")) {
        let (Some(name), Some(value)) = (kw.attribute("name"), kw.attribute("value")) else {
            continue;
        };
        hdr.insert(name, HeaderValue::Str(value.to_string()));
    }
    hdr.insert(
        "FILENAME",
        HeaderValue::Str(pathutil::basename(path).to_string()),
    );

    // 1. instrument:gain. Note the condition is a bare `not in` -- unlike the
    //    fallbacks below it does not also treat ''/'nan'/'None' as missing.
    if hdr.get("GAIN").is_none() {
        if let Some(text) = property_text(&root, "instrument:gain") {
            match text.trim().parse::<f64>() {
                // A decimal below 1 is an e/ADU figure wearing the wrong name.
                Ok(val) if val > 0.0 && val < 1.0 => {
                    hdr.insert("EGAIN", HeaderValue::Str(text.to_string()))
                }
                Ok(_) => hdr.insert("GAIN", HeaderValue::Str(text.to_string())),
                // `float()` raised: keep the raw text as the gain.
                Err(_) => hdr.insert("GAIN", HeaderValue::Str(text.to_string())),
            }
        }
    }

    let filename = pathutil::basename(path);
    // 2 and 3. The filename fallbacks, applied only if still unset.
    if is_unset(&hdr, "GAIN") {
        if let Some(g) = find_after_keyword(filename, "GAIN", MatchKind::Digits) {
            hdr.insert("GAIN", HeaderValue::Str(g));
        }
    }
    if is_unset(&hdr, "FILTER") {
        if let Some(f) = find_after_keyword(filename, "FILTER", MatchKind::UntilUnderscoreOrDot) {
            hdr.insert("FILTER", HeaderValue::Str(f));
        }
    }

    // 4. NUMBER: the integration count, or 1.
    let mut number = 1i64;
    if let Some(text) = property_text(&root, "PixInsight:ProcessingHistory") {
        if let Ok(inner) = roxmltree::Document::parse(&text) {
            if let Some(table) = inner
                .root_element()
                .descendants()
                .find(|n| n.has_tag_name("table") && n.attribute("id") == Some("images"))
            {
                if let Some(rows) = table.attribute("rows") {
                    number = rows.trim().parse::<i64>().unwrap_or(1);
                }
            }
        }
    }
    if number == 1 {
        for kw in root.descendants().filter(|n| is_xisf(n, "FITSKeyword")) {
            let name = kw.attribute("name").unwrap_or("");
            let comment = kw.attribute("comment").unwrap_or("");
            if matches!(name, "COMMENT" | "HISTORY")
                && comment.contains("ImageIntegration.numberOfImages:")
            {
                // `int(comment.split(':')[-1].strip())`
                if let Some(tail) = comment.rsplit(':').next() {
                    if let Ok(n) = tail.trim().parse::<i64>() {
                        number = n;
                    }
                }
                break;
            }
        }
    }
    hdr.insert("NUMBER", HeaderValue::Int(number));
    Ok(hdr)
}

fn is_xisf(node: &roxmltree::Node, tag: &str) -> bool {
    node.is_element() && node.tag_name().name() == tag && node.tag_name().namespace() == Some(NS)
}

/// `root.find(".//xisf:Property[@id='<id>']").text` — the element's text, or
/// `None` when there is no such property or it is empty.
fn property_text<'a>(root: &roxmltree::Node<'a, 'a>, id: &str) -> Option<String> {
    let node = root
        .descendants()
        .find(|n| is_xisf(n, "Property") && n.attribute("id") == Some(id))?;
    // ElementTree's `.text` is the text up to the first child element; these
    // properties have no children, so the whole text content is the same
    // thing. An empty `.text` is falsy on the Python side, hence the filter.
    let text = node.text()?;
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

enum MatchKind {
    /// `GAIN[_-]?(\d+)` — an optional separator, then digits.
    Digits,
    /// `FILTER[_-]([^_.]+)` — a required separator, then anything but `_` or `.`.
    UntilUnderscoreOrDot,
}

/// The two filename fallbacks, transcribed rather than run through a regex
/// engine. Both are `re.search`, so they scan left to right for the first
/// match, and both are case-insensitive on the keyword only.
fn find_after_keyword(name: &str, keyword: &str, kind: MatchKind) -> Option<String> {
    let lower = name.to_lowercase();
    let needle = keyword.to_lowercase();
    let mut from = 0usize;
    while let Some(rel) = lower[from..].find(&needle) {
        let start = from + rel + needle.len();
        let rest = &name[start..];
        let mut chars = rest.chars();
        match kind {
            MatchKind::Digits => {
                // The separator is optional, so skip at most one.
                let body = match chars.next() {
                    Some('_') | Some('-') => &rest[1..],
                    _ => rest,
                };
                let digits: String = body.chars().take_while(|c| c.is_ascii_digit()).collect();
                if !digits.is_empty() {
                    return Some(digits);
                }
            }
            MatchKind::UntilUnderscoreOrDot => {
                if matches!(chars.next(), Some('_') | Some('-')) {
                    let body = &rest[1..];
                    let value: String =
                        body.chars().take_while(|&c| c != '_' && c != '.').collect();
                    if !value.is_empty() {
                        return Some(value);
                    }
                }
            }
        }
        from = from + rel + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gain_fallback_takes_digits_with_an_optional_separator() {
        let f = |n: &str| find_after_keyword(n, "GAIN", MatchKind::Digits);
        assert_eq!(f("masterBias_BIN-1_GAIN-100.xisf").as_deref(), Some("100"));
        assert_eq!(f("x_gain_50_y.xisf").as_deref(), Some("50"));
        assert_eq!(f("Gain100.xisf").as_deref(), Some("100"));
        assert_eq!(f("nothing.xisf"), None);
        // `re.search` keeps scanning: the first GAIN has no digits after it.
        assert_eq!(f("GAIN_x_GAIN-7.xisf").as_deref(), Some("7"));
    }

    #[test]
    fn the_filter_fallback_requires_a_separator_and_stops_at_underscore_or_dot() {
        let f = |n: &str| find_after_keyword(n, "FILTER", MatchKind::UntilUnderscoreOrDot);
        assert_eq!(f("masterFlat_FILTER-Ha_mono_GAIN-100.xisf").as_deref(), Some("Ha"));
        assert_eq!(f("a_Filter_OIII.xisf").as_deref(), Some("OIII"));
        // No separator, so no match.
        assert_eq!(f("FILTERHa.xisf"), None);
    }

    #[test]
    fn unset_covers_the_three_sentinels_the_python_checks() {
        let mut r = Record::default();
        assert!(is_unset(&r, "GAIN"));
        for s in ["", "  ", "nan", "None"] {
            r.insert("GAIN", HeaderValue::Str(s.into()));
            assert!(is_unset(&r, "GAIN"), "{s:?}");
        }
        r.insert("GAIN", HeaderValue::Str("100".into()));
        assert!(!is_unset(&r, "GAIN"));
    }
}

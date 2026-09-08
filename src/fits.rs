//! A FITS **header** reader — hand-written, no cfitsio.
//!
//! This program never touches pixel data, so the whole job is: walk the HDUs,
//! parse 80-column card images, and pick the right header. That is a few
//! hundred lines with no dependencies, and it stops at the first `END` rather
//! than mapping a 122 MB data unit.
//!
//! Two rules beyond the basic format, both from remediation A7:
//!
//! - **HDU selection** is "the first HDU whose header contains `IMAGETYP`",
//!   falling back to HDU 0. Not unconditionally HDU 0, and not filtered by
//!   `XTENSION` — a tile-compressed image keeps its metadata on a BINTABLE.
//! - **Compressed headers are translated, not read literally.** astropy
//!   presents a `CompImageHDU` as the image it decompresses to, and since the
//!   frame's columns come from whatever astropy handed back, this reader has
//!   to do the same. See [`translate_compressed`].
//!
//! Not implemented, because nothing in the corpus contains one and a wrong
//! guess is worse than a loud absence: `CONTINUE` long-string cards and
//! `HIERARCH` keywords. Measured across all 227 FITS files in
//! `parity/fixtures/binary/`, the only non-value cards are `END`, blank
//! padding, `HISTORY` and `COMMENT`.

use anyhow::{bail, Context, Result};

use crate::extractor::{fit_number, HeaderValue, Record};
use crate::pathutil;

const BLOCK: usize = 2880;
const CARD: usize = 80;

/// `_read_fits`: the selected HDU's header, plus `FILENAME` and `NUMBER`.
pub fn read_fits(path: &str) -> Result<Record> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {path}"))?;
    let hdus = read_headers(&bytes)?;
    if hdus.is_empty() {
        bail!("no HDU header found");
    }

    // "Scan HDUs in order and take the first whose header contains IMAGETYP;
    // fall back to HDU 0 if none does."
    let selected = hdus
        .iter()
        .find(|h| h.contains("IMAGETYP"))
        .unwrap_or(&hdus[0]);

    let mut hdr = selected.clone();
    hdr.insert("FILENAME", HeaderValue::Str(pathutil::basename(path).to_string()));
    let number = fit_number(&hdr);
    hdr.insert("NUMBER", HeaderValue::Int(number));
    Ok(hdr)
}

/// Every HDU's header, in file order, each already translated if compressed.
fn read_headers(bytes: &[u8]) -> Result<Vec<Record>> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset + BLOCK <= bytes.len() {
        let (cards, next) = parse_header(bytes, offset)?;
        let record = translate_compressed(cards);
        out.push(record);
        offset = next;
        let Some(data) = data_unit_size(out.last().expect("just pushed")) else {
            break;
        };
        offset += data;
        // A truncated fixture has no data unit at all; that is the end of it.
        if offset + BLOCK > bytes.len() {
            break;
        }
    }
    Ok(out)
}

/// Reads whole 2880-byte blocks of card images until the `END` card.
///
/// Returns the header and the offset of the block after `END`.
fn parse_header(bytes: &[u8], start: usize) -> Result<(Record, usize)> {
    let mut record = Record::default();
    let mut offset = start;
    loop {
        if offset + BLOCK > bytes.len() {
            bail!("header runs past the end of the file");
        }
        let block = &bytes[offset..offset + BLOCK];
        offset += BLOCK;
        for i in 0..(BLOCK / CARD) {
            let card = &block[i * CARD..(i + 1) * CARD];
            let text = String::from_utf8_lossy(card);
            let keyword = text[..8.min(text.len())].trim();
            if keyword == "END" {
                return Ok((record, offset));
            }
            if keyword.is_empty() {
                continue; // blank padding, and the blank commentary keyword
            }
            if keyword == "HISTORY" || keyword == "COMMENT" {
                // Commentary cards repeat, so they accumulate rather than
                // overwrite. astropy hands these back as one list-like object
                // per keyword; `_get_fit_number` iterates its lines.
                let line = text[8..].trim_end().to_string();
                let line = line.strip_prefix(' ').unwrap_or(&line).to_string();
                match record.values.get_mut(keyword) {
                    Some(HeaderValue::Commentary(lines)) => lines.push(line),
                    _ => record.insert(keyword, HeaderValue::Commentary(vec![line])),
                }
                continue;
            }
            if &text[8..10] != "= " {
                // A keyword with no value indicator is a commentary card of
                // some other name; astropy keeps it, but nothing in the
                // corpus has one.
                continue;
            }
            if let Some(value) = parse_value(&text[10..]) {
                record.insert(keyword, value);
            }
        }
    }
}

/// The value field of a card: everything after `= `, up to an unquoted `/`.
///
/// astropy's rules, in its order: a quoted string (with `''` for an embedded
/// quote and trailing blanks stripped), then `T`/`F`, then an integer, then a
/// float — where a Fortran `D` exponent counts as `E`.
fn parse_value(field: &str) -> Option<HeaderValue> {
    let trimmed = field.trim_start();
    if let Some(rest) = trimmed.strip_prefix('\'') {
        let mut value = String::new();
        let mut chars = rest.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    chars.next();
                    value.push('\'');
                } else {
                    break;
                }
            } else {
                value.push(c);
            }
        }
        // FITS pads string values to at least eight characters, and astropy
        // strips the padding back off.
        return Some(HeaderValue::Str(value.trim_end().to_string()));
    }

    // Strip the comment, which starts at the first `/` outside a string --
    // and there is no string here, having handled that case above.
    let body = match trimmed.split_once('/') {
        Some((v, _)) => v.trim(),
        None => trimmed.trim(),
    };
    if body.is_empty() {
        // An undefined value. astropy carries an `Undefined` sentinel, whose
        // `str()` is empty; no card in the corpus is undefined, so this is
        // the one place a divergence could hide.
        return Some(HeaderValue::Str(String::new()));
    }
    match body {
        "T" => return Some(HeaderValue::Bool(true)),
        "F" => return Some(HeaderValue::Bool(false)),
        _ => {}
    }
    if is_integer(body) {
        if let Ok(i) = body.parse::<i64>() {
            return Some(HeaderValue::Int(i));
        }
    }
    let normalised = body.replace(['D', 'd'], "E");
    if let Ok(f) = normalised.parse::<f64>() {
        return Some(HeaderValue::Float(f));
    }
    Some(HeaderValue::Str(body.to_string()))
}

fn is_integer(s: &str) -> bool {
    let body = s.strip_prefix(['+', '-']).unwrap_or(s);
    !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit())
}

/// The data unit's size in bytes, padded to whole blocks:
/// `|BITPIX|/8 * GCOUNT * (PCOUNT + NAXIS1 * ... * NAXISn)`.
///
/// `None` when the header does not describe one, which ends the walk.
fn data_unit_size(hdr: &Record) -> Option<usize> {
    let int_of = |k: &str| match hdr.get(k) {
        Some(HeaderValue::Int(i)) => Some(*i),
        _ => None,
    };
    let bitpix = int_of("BITPIX")?;
    let naxis = int_of("NAXIS")?;
    if naxis == 0 {
        return Some(0);
    }
    let mut n: i64 = 1;
    for i in 1..=naxis {
        n = n.checked_mul(int_of(&format!("NAXIS{i}"))?)?;
    }
    let pcount = int_of("PCOUNT").unwrap_or(0);
    let gcount = int_of("GCOUNT").unwrap_or(1);
    let size = (bitpix.abs() / 8).checked_mul(gcount)?.checked_mul(pcount + n)?;
    if size <= 0 {
        return Some(0);
    }
    let size = size as usize;
    Some(size.div_ceil(BLOCK) * BLOCK)
}

/// astropy presents a tile-compressed BINTABLE as the image it decompresses
/// to, and the raw frame's columns are whatever astropy handed back — so this
/// reader has to perform the same substitution rather than reporting what is
/// literally on disk.
///
/// Measured against astropy on `synthetic/01_compressed.fits`: `XTENSION`
/// becomes `IMAGE`, the `Z`-prefixed originals replace the table's own
/// geometry, and every compression-machinery card disappears, including
/// `EXTNAME` — which a plain `ImageHDU` keeps.
///
/// ```text
///   on disk                     as astropy presents it
///   XTENSION= 'BINTABLE'   ->   XTENSION= 'IMAGE'
///   BITPIX  =            8 ->   BITPIX  =           16   (from ZBITPIX)
///   NAXIS   =            2 ->   NAXIS   =            2   (from ZNAXIS)
///   NAXIS1  =            8 ->   NAXIS1  =            2   (from ZNAXIS1)
///   NAXIS2  =            2 ->   NAXIS2  =            2   (from ZNAXIS2)
///   PCOUNT  =            6 ->   PCOUNT  =            0   (from ZPCOUNT)
///   GCOUNT  =            1 ->   GCOUNT  =            1   (from ZGCOUNT)
///   TFIELDS, TTYPE1, TFORM1, ZIMAGE, ZTENSION, ZBITPIX, ZNAXIS*, ZPCOUNT,
///   ZGCOUNT, ZTILE*, ZCMPTYPE, ZNAME*, ZVAL*, EXTNAME  ->  dropped
/// ```
///
/// None of this reaches the acquisition CSV or the summary — the user cards
/// pass through untouched either way — but it does decide the raw frame's
/// column set, which is what `--dump-steps` compares.
fn translate_compressed(hdr: Record) -> Record {
    let compressed = matches!(hdr.get("ZIMAGE"), Some(HeaderValue::Bool(true)));
    if !compressed {
        return hdr;
    }

    let z = |name: &str| hdr.get(name).cloned();
    let mut out = Record::default();
    for key in &hdr.keys {
        let value = hdr.values.get(key).expect("key came from keys").clone();
        match key.as_str() {
            "XTENSION" => out.insert("XTENSION", HeaderValue::Str("IMAGE".into())),
            "BITPIX" => out.insert("BITPIX", z("ZBITPIX").unwrap_or(value)),
            "NAXIS" => out.insert("NAXIS", z("ZNAXIS").unwrap_or(value)),
            k if is_indexed(k, "NAXIS") => {
                let idx = &k["NAXIS".len()..];
                out.insert(k, z(&format!("ZNAXIS{idx}")).unwrap_or(value));
            }
            "PCOUNT" => out.insert("PCOUNT", z("ZPCOUNT").unwrap_or(value)),
            "GCOUNT" => out.insert("GCOUNT", z("ZGCOUNT").unwrap_or(value)),
            k if is_compression_card(k) => {}
            k => out.insert(k, value),
        }
    }
    out
}

/// `NAXIS1`, `ZVAL2`, `TTYPE1` — a fixed prefix followed by digits.
fn is_indexed(key: &str, prefix: &str) -> bool {
    key.len() > prefix.len()
        && key.starts_with(prefix)
        && key[prefix.len()..].bytes().all(|b| b.is_ascii_digit())
}

/// The compression machinery astropy hides. Anything outside this list is
/// passed through, so an unrecognised `Z`-card would show up as a column
/// difference in the dump rather than silently changing an output value.
fn is_compression_card(key: &str) -> bool {
    matches!(
        key,
        "TFIELDS"
            | "THEAP"
            | "EXTNAME"
            | "ZIMAGE"
            | "ZTENSION"
            | "ZBITPIX"
            | "ZNAXIS"
            | "ZPCOUNT"
            | "ZGCOUNT"
            | "ZCMPTYPE"
            | "ZQUANTIZ"
            | "ZDITHER0"
            | "ZSIMPLE"
            | "ZEXTEND"
            | "ZBLANK"
            | "ZSCALE"
            | "ZZERO"
            | "ZHECKSUM"
            | "ZDATASUM"
    ) || is_indexed(key, "TTYPE")
        || is_indexed(key, "TFORM")
        || is_indexed(key, "TSCAL")
        || is_indexed(key, "TZERO")
        || is_indexed(key, "ZNAXIS")
        || is_indexed(key, "ZTILE")
        || is_indexed(key, "ZNAME")
        || is_indexed(key, "ZVAL")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(field: &str) -> HeaderValue {
        parse_value(field).unwrap()
    }

    #[test]
    fn card_values_take_astropys_types() {
        assert_eq!(v("                    T / comment"), HeaderValue::Bool(true));
        assert_eq!(v("                    F"), HeaderValue::Bool(false));
        assert_eq!(v("                  100 / gain"), HeaderValue::Int(100));
        assert_eq!(v("                600.0"), HeaderValue::Float(600.0));
        assert_eq!(v("                 -10.0"), HeaderValue::Float(-10.0));
        // A Fortran D exponent is an E exponent.
        assert_eq!(v("            1.5D2"), HeaderValue::Float(150.0));
    }

    #[test]
    fn string_values_lose_their_padding_and_keep_their_slashes() {
        assert_eq!(v("'LIGHT   '"), HeaderValue::Str("LIGHT".into()));
        assert_eq!(v("'NP101is '  / telescope"), HeaderValue::Str("NP101is".into()));
        // A `/` inside quotes is content, not the start of a comment.
        assert_eq!(v("'a/b'"), HeaderValue::Str("a/b".into()));
        // Two quotes stand for one.
        assert_eq!(v("'it''s'"), HeaderValue::Str("it's".into()));
        assert_eq!(v("''"), HeaderValue::Str(String::new()));
    }

    #[test]
    fn the_data_unit_formula_puts_pcount_inside_the_group_multiply() {
        let mut h = Record::default();
        for (k, n) in [("BITPIX", 16), ("NAXIS", 2), ("NAXIS1", 100), ("NAXIS2", 100)] {
            h.insert(k, HeaderValue::Int(n));
        }
        // 2 bytes * 1 group * (0 + 10000) = 20000 -> 7 blocks
        assert_eq!(data_unit_size(&h), Some(20160));
        h.insert("PCOUNT", HeaderValue::Int(4));
        h.insert("GCOUNT", HeaderValue::Int(3));
        // 2 * 3 * (4 + 10000) = 60024 -> 21 blocks
        assert_eq!(data_unit_size(&h), Some(60480));
        h.insert("NAXIS", HeaderValue::Int(0));
        assert_eq!(data_unit_size(&h), Some(0));
    }

    #[test]
    fn a_compressed_header_is_presented_as_the_image_it_decompresses_to() {
        let mut h = Record::default();
        for (k, val) in [
            ("XTENSION", HeaderValue::Str("BINTABLE".into())),
            ("BITPIX", HeaderValue::Int(8)),
            ("NAXIS", HeaderValue::Int(2)),
            ("NAXIS1", HeaderValue::Int(8)),
            ("NAXIS2", HeaderValue::Int(2)),
            ("PCOUNT", HeaderValue::Int(6)),
            ("GCOUNT", HeaderValue::Int(1)),
            ("TFIELDS", HeaderValue::Int(1)),
            ("TTYPE1", HeaderValue::Str("COMPRESSED_DATA".into())),
            ("ZIMAGE", HeaderValue::Bool(true)),
            ("ZBITPIX", HeaderValue::Int(16)),
            ("ZNAXIS", HeaderValue::Int(2)),
            ("ZNAXIS1", HeaderValue::Int(2)),
            ("ZNAXIS2", HeaderValue::Int(2)),
            ("ZPCOUNT", HeaderValue::Int(0)),
            ("EXTNAME", HeaderValue::Str("COMPRESSED_IMAGE".into())),
            ("IMAGETYP", HeaderValue::Str("LIGHT".into())),
        ] {
            h.insert(k, val);
        }
        let out = translate_compressed(h);
        assert_eq!(
            out.keys,
            ["XTENSION", "BITPIX", "NAXIS", "NAXIS1", "NAXIS2", "PCOUNT", "GCOUNT", "IMAGETYP"]
        );
        assert_eq!(out.get("XTENSION"), Some(&HeaderValue::Str("IMAGE".into())));
        assert_eq!(out.get("BITPIX"), Some(&HeaderValue::Int(16)));
        assert_eq!(out.get("NAXIS1"), Some(&HeaderValue::Int(2)));
        assert_eq!(out.get("PCOUNT"), Some(&HeaderValue::Int(0)));
        // GCOUNT had no ZGCOUNT to take from, so it keeps its own value.
        assert_eq!(out.get("GCOUNT"), Some(&HeaderValue::Int(1)));
    }

    #[test]
    fn an_uncompressed_header_passes_through_untouched() {
        let mut h = Record::default();
        h.insert("EXTNAME", HeaderValue::Str("SCIENCE".into()));
        h.insert("TFIELDS", HeaderValue::Int(3));
        let out = translate_compressed(h);
        assert_eq!(out.keys, ["EXTNAME", "TFIELDS"]);
    }
}

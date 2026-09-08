//! `OpticalParameterStep` — HFR, image scale and FWHM for light frames.
//!
//! Only light frames are touched; every other row keeps whatever Stage 7 left
//! in `hfr`, `imscale` and `fwhm`.
//!
//! The rounding here is **CPython's builtin `round`**, not pandas' — see
//! `numeric::python_round`. The two disagree at exact decimal boundaries and
//! the disagreement survives into the printed summary (A12 upstream), so the
//! choice is load-bearing rather than stylistic.

use anyhow::Result;

use crate::appconfig::AppConfig;
use crate::constants as col;
use crate::constants::image_type;
use crate::numeric::python_round;
use crate::steps::{astype_str, to_numeric};
use crate::table::{Cell, Table};

/// arcsec/pixel = (pixel size in microns / focal length in mm) × this.
const ARCSEC_PER_RADIAN: f64 = 206.265;

/// FWHM is approximated as twice the HFR — a rule of thumb, not a derivation.
const FWHM_TO_HFR_RATIO: f64 = 2.0;

pub fn execute(table: &Table, cfg: &AppConfig) -> Result<Table> {
    crate::log_info!(
        "execute",
        67,
        "Processing optical parameters and calculating star metrics"
    );
    let mut df = table.clone();
    if df.n_rows == 0 {
        return Ok(df);
    }

    let hfr_default = cfg.default_f64("HFR", 1.0)?;

    let Some(itype) = df.column(col::IMAGE_TYPE) else {
        return Ok(df);
    };
    let lights: Vec<usize> = itype
        .cells
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c, Cell::Str(s) if s == image_type::LIGHT))
        .map(|(i, _)| i)
        .collect();
    if lights.is_empty() {
        return Ok(df);
    }

    let filenames = df.column(col::FILENAME);
    let focallen = df.column(col::FOCAL_LENGTH);
    let xpixsz = df.column(col::PIXEL_SIZE);

    let mut updates: Vec<(usize, f64, f64, f64)> = Vec::with_capacity(lights.len());
    for &i in &lights {
        // 1. HFR from the filename, where the capture software put it there.
        //    No match, or a non-positive value, both fall back to the config
        //    default -- `.where()` treats a NaN condition as False, so the two
        //    paths are one.
        let extracted = filenames
            .map(|c| astype_str(&c.cells[i]))
            .and_then(|name| extract_hfr(&name))
            .and_then(|text| crate::table::parse_float(&text));
        let hfr = match extracted {
            Some(v) if v > 0.0 => v,
            _ => hfr_default,
        };

        // 2. Image scale. A focal length that is missing, unparseable or
        //    non-positive falls back to 1.0; a missing *pixel size* does not,
        //    it propagates as NaN, exactly as the original guard did.
        let flen = focallen.and_then(|c| to_numeric(&c.cells[i]));
        let pix = xpixsz.and_then(|c| to_numeric(&c.cells[i]));
        let imscale = match flen {
            Some(f) if f > 0.0 => pix.unwrap_or(f64::NAN) / f * ARCSEC_PER_RADIAN,
            _ => 1.0,
        };

        // 3. FWHM.
        let fwhm = if hfr >= 0.0 {
            hfr * imscale * FWHM_TO_HFR_RATIO
        } else {
            0.0
        };

        updates.push((
            i,
            python_round(hfr, 2),
            python_round(imscale, 2),
            python_round(fwhm, 2),
        ));
    }

    for (name, pick) in [
        (col::HFR, 1usize),
        (col::IMSCALE, 2),
        (col::MEAN_FWHM, 3),
    ] {
        if let Some(column) = df.column_mut(name) {
            for u in &updates {
                let v = match pick {
                    1 => u.1,
                    2 => u.2,
                    _ => u.3,
                };
                column.cells[u.0] = Cell::Float(v);
            }
        }
    }

    crate::log_debug!(
        "execute",
        119,
        "Computed optical metrics for {} light frame(s).",
        lights.len()
    );
    Ok(df)
}

/// `str.extract(r'HFR_([0-9.]+)')` — the first `HFR_` in the name, then the
/// run of digits and dots after it. No match yields `None`, which the caller
/// treats as the NaN the Python side gets.
fn extract_hfr(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    let mut start = 0usize;
    while let Some(pos) = name[start..].find("HFR_") {
        let digits_at = start + pos + 4;
        let end = bytes[digits_at..]
            .iter()
            .position(|b| !(b.is_ascii_digit() || *b == b'.'))
            .map_or(bytes.len(), |n| digits_at + n);
        if end > digits_at {
            return Some(name[digits_at..end].to_string());
        }
        // `[0-9.]+` needs at least one character; a bare 'HFR_' is not a
        // match, and the search continues after it.
        start = digits_at;
        if start >= name.len() {
            break;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hfr_comes_out_of_the_filename() {
        assert_eq!(
            extract_hfr("Sadr_Exposure_600.00s_HFR_1.63pxs_FrameNo_0002.fits").as_deref(),
            Some("1.63")
        );
        assert_eq!(extract_hfr("no_metrics_here.fits"), None);
        // A bare marker with nothing numeric after it is not a match.
        assert_eq!(extract_hfr("HFR_pxs.fits"), None);
        // Trailing dots are part of the class, as in the Python pattern.
        assert_eq!(extract_hfr("x_HFR_1.6.3_y.fits").as_deref(), Some("1.6.3"));
    }

    #[test]
    fn imscale_and_fwhm_are_derived_only_for_lights() {
        let cfg = AppConfig::from_config(
            &crate::config::ConfigFile::parse_str("[defaults]\nHFR = 1\n").unwrap(),
        )
        .unwrap();
        let df = Table::parse_str(
            "imagetyp,filename,focallen,xpixsz,hfr,imscale,fwhm\n\
             LIGHT,a_HFR_2.00pxs.fits,540.0,3.76,1.0,1.0,0.0\n\
             DARK,b.fits,540.0,3.76,9.0,9.0,9.0\n",
        )
        .unwrap();
        let out = execute(&df, &cfg).unwrap();
        // 3.76 / 540 * 206.265 = 1.4362... -> 1.44
        assert_eq!(out.column("imscale").unwrap().cells[0], Cell::Float(1.44));
        assert_eq!(out.column("hfr").unwrap().cells[0], Cell::Float(2.0));
        // 2.00 * 1.4362... * 2 = 5.744... -> 5.74 (rounded from the
        // *unrounded* image scale, not from 1.44)
        assert_eq!(out.column("fwhm").unwrap().cells[0], Cell::Float(5.74));
        // The dark row is untouched.
        assert_eq!(out.column("imscale").unwrap().cells[1], Cell::Float(9.0));
    }

    #[test]
    fn a_non_positive_focal_length_falls_back_to_unit_image_scale() {
        let cfg = AppConfig::default();
        let df = Table::parse_str(
            "imagetyp,filename,focallen,xpixsz,hfr,imscale,fwhm\n\
             LIGHT,a.fits,0.0,3.76,1.0,1.0,0.0\n",
        )
        .unwrap();
        let out = execute(&df, &cfg).unwrap();
        assert_eq!(out.column("imscale").unwrap().cells[0], Cell::Float(1.0));
    }
}

#!/usr/bin/env python3
"""Generates parity/fixtures/binary/synthetic/ -- FITS branches no real file reaches.

Everything else in the binary corpus is a header-only truncation of a real
capture, deliberately: a hand-built corpus only tests the cases its author
thought of, which is how remediation A7's tile-compressed bug survived. These
five files are the exception, because the machine that holds the rest of the
corpus has no `.fits.fz` anywhere on it and the HDU-selection rule is the
easiest thing in the whole FITS reader to get quietly wrong.

Each file isolates one rule of `_read_fits`:

  01_compressed.fits      a tile-compressed image: the real metadata sits in a
                          BINTABLE with ZIMAGE=T, leaving HDU 0 structural
                          boilerplate. The reader must walk every HDU header,
                          not filter by XTENSION. This is remediation A7's bug.
  02_imagetyp_hdu2.fits   IMAGETYP is in HDU 2 and absent from 0 and 1 -- so
                          "first HDU carrying IMAGETYP", not "HDU 1".
  03_no_imagetyp.fits     no HDU carries IMAGETYP; falls back to HDU 0 so
                          [defaults] still applies.
  04_master_history.fits  ImageIntegration.numberOfImages spread across
                          repeated HISTORY cards. The reader must expose those
                          as a sequence -- last-write-wins loses the count.
  05_quoted_values.fits   string values whose quotes and padding the extractor
                          strips (`hdr[k].strip("'").strip('"')`).
  06_compressed.fits.fz   byte-identical to 01, named `.fits.fz`. The traversal
                          filter is `('.fits', '.fit', '.fts', '.xisf')`, so
                          this file is **silently skipped** -- a genuinely
                          compressed capture with the conventional extension
                          never reaches the reader at all. PORT_PLAN.md called
                          for "a tile-compressed .fits.fz case"; measured, that
                          case tests the *filter*, and the case that tests the
                          *reader* is 01. Both are pinned so the Rust traversal
                          reproduces the exclusion rather than improving on it.

The pixel data is a 2x2 zero array: the pipeline never reads it, and keeping
it minimal keeps every file under 20 KiB.

    python3 parity/make_synthetic_fits.py

Unlike make_binary_fixtures.py this needs no data outside the repository, so
it runs anywhere astropy is installed. Run it after that script, which clears
the binary corpus directory.
"""

import hashlib
import json
import pathlib

import numpy as np
from astropy.io import fits

HERE = pathlib.Path(__file__).resolve().parent
OUT = HERE / "fixtures" / "binary" / "synthetic"
MANIFEST = HERE / "fixtures" / "binary" / "MANIFEST.json"

DATA = np.zeros((2, 2), dtype=np.int16)

# Enough of a real N.I.N.A. header for the pipeline to produce a row, borrowed
# in shape (not in values) from the Sadr captures.
BASE = {
    "IMAGETYP": "LIGHT",
    "EXPOSURE": 600.0,
    "DATE-OBS": "2026-03-01T22:14:03.123",
    "INSTRUME": "ZWO ASI6200MM Pro",
    "TELESCOP": "NP101is",
    "GAIN": 100,
    "EGAIN": 0.2466576397418976,
    "XBINNING": 1,
    "CCD-TEMP": -10.0,
    "XPIXSZ": 3.76,
    "FOCALLEN": 540.0,
    "FOCRATIO": 5.4,
    "SITELAT": 52.24838888888889,
    "SITELONG": -0.12313888888888889,
    "FILTER": "Ha",
    "OBJECT": "Synthetic Target",
    "FOCTEMP": 11.5,
    "SWCREATE": "N.I.N.A. 3.2.0.9001 (x64)",
}


def apply(header, cards):
    for k, v in cards.items():
        header[k] = v


def compressed():
    """A .fits.fz: metadata on the compressed extension, not the primary HDU."""
    primary = fits.PrimaryHDU()  # SIMPLE/BITPIX/NAXIS/EXTEND only
    comp = fits.CompImageHDU(data=DATA, compression_type="RICE_1")
    apply(comp.header, BASE)
    return fits.HDUList([primary, comp])


def imagetyp_hdu2():
    primary = fits.PrimaryHDU(DATA)
    # HDU 1 carries plausible-looking cards but no IMAGETYP.
    first = fits.ImageHDU(DATA, name="PREVIEW")
    first.header["EXPOSURE"] = 1.0
    first.header["FILTER"] = "Decoy"
    second = fits.ImageHDU(DATA, name="SCIENCE")
    apply(second.header, BASE)
    return fits.HDUList([primary, first, second])


def no_imagetyp():
    primary = fits.PrimaryHDU(DATA)
    cards = {k: v for k, v in BASE.items() if k != "IMAGETYP"}
    apply(primary.header, cards)
    return fits.HDUList([primary])


def master_history():
    primary = fits.PrimaryHDU(DATA)
    apply(primary.header, {**BASE, "IMAGETYP": "MASTERDARK"})
    for line in (
        "ImageIntegration.numberOfImages: 50",
        "ImageIntegration.totalPixels: 244735104",
        "ImageIntegration.outputRangeLow: 0.0000000000",
    ):
        primary.header["HISTORY"] = line
    primary.header["COMMENT"] = "not the count: ImageIntegration.numberOfImages appears above"
    return fits.HDUList([primary])


def quoted_values():
    primary = fits.PrimaryHDU(DATA)
    apply(primary.header, BASE)
    # astropy writes string values inside single quotes and pads them to eight
    # characters; the extractor strips both layers back off.
    primary.header["FILTER"] = "'Ha'"
    primary.header["OBJECT"] = '"Quoted Target"'
    primary.header["TELESCOP"] = "NP101is   "
    return fits.HDUList([primary])


CASES = [
    ("01_compressed.fits", compressed, "metadata on a ZIMAGE=T BINTABLE, HDU 0 is boilerplate"),
    ("02_imagetyp_hdu2.fits", imagetyp_hdu2, "IMAGETYP in HDU 2, absent from 0 and 1"),
    ("03_no_imagetyp.fits", no_imagetyp, "no HDU carries IMAGETYP; falls back to HDU 0"),
    ("04_master_history.fits", master_history, "numberOfImages in repeated HISTORY cards"),
    ("05_quoted_values.fits", quoted_values, "string values the extractor unquotes"),
    ("06_compressed.fits.fz", compressed,
     "same file, .fz extension: skipped by the traversal filter, never read"),
]


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    entries = []
    for name, build, why in CASES:
        path = OUT / name
        if path.exists():
            path.unlink()
        build().writeto(path, output_verify="exception")
        entries.append({
            "path": f"synthetic/{name}",
            "bytes": path.stat().st_size,
            "why": why,
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
        })
        print(f"{name:24} {path.stat().st_size / 1024:7.1f} KiB  {why}")

    if MANIFEST.exists():
        manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
        manifest["scenarios"]["synthetic"]["files"] = entries
        MANIFEST.write_text(json.dumps(manifest, indent=1) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

# Parity corpus — provenance

The corpus has **two halves**, and the difference matters: one is copied from
upstream and must be kept identical to it, the other was captured for this
port and has no upstream counterpart.

## Copied from upstream

- Source: <https://github.com/SteveGreaves/AstroBinUploader> `golden_tests/`
- Taken at: `d2f61bcb17ed2fc6ad76c3b400318f651a2f5408` (`v2.1.1-5-gd2f61bc`)
- Copied: 2026-09-07

| Here | Upstream |
|---|---|
| `fixtures/sadr_raw.csv`, `sadr.basename` | `golden_tests/fixtures/` |
| `fixtures/sh2101_calib_raw.csv`, `sh2101_calib.basename` | `golden_tests/fixtures/` |
| `references/sadr_*`, `references/sh2101_calib_*` | `golden_tests/references/` |
| `golden_config.ini` | `golden_tests/golden_config.ini` |

They are duplicated rather than referenced so this repository builds and tests
on its own, with no second checkout and no submodule.
`check_steps.py::check_oracle` verifies these copies still match the sibling
checkout on every run.

### When to re-copy

Whenever the Python side re-blesses its references — any change to Bucket A
behaviour, or to `golden_config.ini` — these copies go stale and the
differential harness starts comparing against the wrong baseline. Re-copy the
paths above and update the commit recorded here in the same change.

## Captured for this port (2026-09-08)

No upstream counterpart, so the staleness check deliberately skips them
(`LOCAL_FIXTURES` in `check_steps.py`). Both were blessed by running live
Python **v2.1.1** over the fixture CSV via `--test`, exactly as upstream's
`run_golden.py --bless` does.

| Fixture | Rows | Source directory | What it adds |
|---|---|---|---|
| `mosaic` | 1544 | `/mnt/raid0/AstroImaging/Preselected/North American Nebula (NGC_6997) Mosaic started July 9th 2025` | Four-panel mosaic — the only fixture exercising `get_target_details`' Panel detection and multiple targets in one LIGHTS table. All three master calibration tables. No `SOURCE_PATH` (captured before A2), so like `sadr` it takes the degraded dedup branch. |
| `lbn548` | 270 | `/mnt/raid0/AstroImaging/Source/LBN 548` | Lights only, no calibration frames of any kind — the only fixture where every `darks`/`flats`/`flatDarks`/`bias` column is zero and the report emits a LIGHTS table and nothing else. Has `SOURCE_PATH`. Note the directory is `LBN 548` while `OBJECT` reads `LBN 542`; that mismatch is in the real data and is deliberately preserved. |

`mosaic_raw.csv` is the `debug_step_00_RawHeaders.csv` the Python side had
already written into that data directory. `lbn548_raw.csv` was captured with

```sh
python3 AstroBinUpload.py "<scratch>/LBN 548" "/mnt/raid0/AstroImaging/Source/LBN 548" \
        --debug --config parity/golden_config.ini
```

— an empty scratch directory as the *first* argument so the output basename is
`LBN_548` and nothing is written into the image tree.

## Removed on 2026-09-08

Four reference pairs were deleted. None had a replayable fixture, so nothing
verified them, and two were provably wrong:

| Removed | Why |
|---|---|
| `flame_*` | **Stale.** Shows `0.10 dB` gains and a `Total MASTERFLAT Exposure Time:` label; v2.1.1 emits an integer gain and `Total FLAT`. Third-party dataset (Salt Lake City site), not on this machine. |
| `alpha_*` | Third-party dataset (same site as `flame`), not on this machine, unverifiable. |
| `michael_*` | Third-party dataset (ASA N12 / TOUPTEK, null coordinates), not on this machine, unverifiable. |
| `lbn548_31may_*` | A second capture of the same LBN 548 dataset differing only in how many files were on disk that day (1625 vs 1608). Superseded by the re-blessed `lbn548`. |

`mosaic_summary.txt` was **re-blessed rather than removed**: the committed copy
predated remediation A14 and showed `Ha` on the MASTERDARKS and MASTERBIAS
rows, where v2.1.1 correctly leaves the filter blank for filter-independent
calibration types. That is exactly the misleading label A14 fixed.

## Binary corpus — `fixtures/binary/` (2026-09-08)

Phase 4's readers need FITS and XISF files to test against, and neither this
repository nor upstream has ever had any (`REMEDIATION_PLAN.md` P0 item 3 was
never done). Built by `make_binary_fixtures.py` and `make_synthetic_fits.py`,
which are committed alongside it; the first needs the maintainer's image
library, the second needs only astropy.

Every real file is a **header-only truncation**. The pipeline reads headers and
never touches pixel data, so cutting the file off after the header loses
nothing — verified: astropy returns an identical header dict from the
truncated copy, and `_read_xisf` only ever reads the XML block. A 122 MB frame
becomes 5.6 KiB, a 734 MB PixInsight master becomes 190 KiB.

| Scenario | Files | Size | What it is for |
|---|---|---|---|
| `Sadr Region/` | 221 | 1.2 MiB | The whole Sadr FITS tree, structure and names preserved. **A live-Python scan of it reproduces `references/sadr_*` byte for byte**, so the FITS reader gets an end-to-end target that already exists. Also the only fixture exercising traversal order over a real four-level tree, and its directory name carries the space that `basename.replace(" ", "_")` has to handle. |
| `xisf_mixed/` | 14 | 1.1 MiB | Lights in two filters, one raw frame of each calibration type, three PixInsight masters (`ImageIntegration.numberOfImages` in HISTORY — and `Master Bias`/`Master Dark` carry no FILTER, so this is the first fixture reaching the blank-filter calibration rows), and WBPP `_c_lps_r` names that collide with their originals under the dedup regex. |
| `synthetic/` | 6 | 56 KiB | Branches no real file on this machine reaches. |

References for the latter two are `references/binary_*`, blessed from a live
v2.1.1 **disk scan** rather than a `--test` replay — the scan is the thing
being tested. `SOURCE_PATH` holds an absolute path and so varies by machine,
but it never reaches either artifact, so the references are portable.

`make_binary_fixtures.py --check` verifies the committed corpus against
`fixtures/binary/MANIFEST.json` without needing the source images.

### The synthetic cases, and one measured surprise

`PORT_PLAN.md` asked for "a tile-compressed `.fits.fz` case". Measured, that is
two different cases, and the plan's phrasing picks the less useful one:

- The traversal filter is `('.fits', '.fit', '.fts', '.xisf')`
  (`extractor.py:88`), so a file actually **named** `.fits.fz` is silently
  skipped and never reaches the reader at all.
- The case remediation A7 was written for is a tile-compressed image inside a
  file named `.fits` — HDU 0 holding only `SIMPLE`/`BITPIX`/`NAXIS`/`EXTEND`
  and the real metadata on a `ZIMAGE = T` BINTABLE extension.

`01_compressed.fits` and `06_compressed.fits.fz` are byte-identical and differ
only in extension, pinning both halves: the reader must walk every HDU header,
and the traversal must reproduce the exclusion rather than improving on it.

Verified against the live extractor: 5 of the 6 files are picked up (`06` is
skipped); `01` and `02` find `IMAGETYP` on HDUs 1 and 2 respectively; `03`
finds none and falls back to HDU 0; `04` reads `NUMBER = 50` out of repeated
HISTORY cards; `05` has its quotes and padding stripped.

## Not yet covered by any fixture

- **`DARKFLAT` / `MASTERDARKFLATS`.** 200 darkflat frames exist at
  `/mnt/raid0/AstroImaging/Preselected/Calibration data/24th February 2022/FlatWizard/`,
  but they need a lights set from the same era and gain to match; the 2025–26
  datasets will not pair with them.
- **A multi-site session.** Every fixture reports exactly one site, so the
  `for site, site_group in df.groupby(...)` loop has never run twice.
- **A blank filter in a flat table** (`'No Filter'` / `'None'`).

## Do not regenerate `fixtures/sadr_raw.csv`

It predates remediation A2 and therefore carries no `SOURCE_PATH` column,
which makes it the only fixture exercising the *degraded* filename-only
deduplication branch (and its warning). `sh2101_calib_raw.csv` exercises the
normal `(directory, basename)` path. Regenerating sadr would silently drop
half that coverage. See `PORT_PLAN.md` hazard 13.

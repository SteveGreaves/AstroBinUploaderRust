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

## `ic405`, captured for this port (2026-09-09)

Closes the `DARKFLAT` gap the section below used to record. Same rules as the
two fixtures above -- no upstream counterpart, listed in `LOCAL_FIXTURES` -- but
blessed against live Python **v2.1.2**, the current parity target.

| Fixture | Rows | Source directory | What it adds |
|---|---|---|---|
| `ic405` | 1365 | `/mnt/raid0/AstroImaging/Preselected/Flaming Star Nebula (IC405) started 4th February 2022` | The only fixture with `IMAGETYP = 'DARKFLAT'` frames (765 LIGHT, 350 FLAT, 250 DARKFLAT), so the only one whose `flatDarks` column is non-zero and whose summary emits a `DARKFLATS:` section and a `Total DARKFLAT Exposure Time` line. Also the only fixture where `GeocodeStep` forms **two** clusters. Has `SOURCE_PATH`, so like `lbn548` it takes the normal `(directory, basename)` dedup branch. |

### The two-cluster geocode, and why the report still says one site

Traced through the step dumps, not inferred. In
`debug_step_00_RawHeaders.csv`, 602 of the 1365 frames carry no `SITELAT` at
all: all 350 FLATs, all 250 DARKFLATs, and **two LIGHTs**. Then, in order:

1. `base.py:225` -- Stage 7, "Core Column Hardening", inside
   `NormalizeHeadersStep` -- fills missing `SITELAT`/`SITELONG` with `0.0`.
   By `debug_step_01` nothing is null any more and the column has gained a
   seventh distinct value.
2. `GeocodeStep._align_coordinates` (`geocode.py:186`) exists to give
   calibration frames a light frame's coordinates, but it decides via
   `pd.isna()`. Nothing is NA by then, so the "Direct fallback" branch never
   fires and every calibration frame takes the geodesic-match branch instead.
3. That branch assigns each frame the coordinates of the **nearest** LIGHT.
   Two LIGHTs now sit at `0, 0`, so for a frame already at `0, 0` the nearest
   light is one of those two, at distance zero. All 600 calibration frames are
   "aligned" onto `0, 0`. The loop iterates over non-LIGHT rows only, so the two
   zero-filled lights are never repaired either.

`0, 0` is some 5800 km outside `CLUSTER_RADIUS_M = 110.0` metres of Papworth
Everard, so the greedy single-linkage pass produces two clusters,
`(52.248426, -0.123102)` and `(0.0, 0.0)` -- visible in
`debug_step_05_GeocodeStep.csv`. Both resolve to the same site *name* through
the database lookup, and the report groups by name, which is why
`ic405_summary.txt` prints one `Site:` block and `check_reports.py` reports
`1 site(s)`. Its `Latitude: 0.0000` / `Longitude: 0.0000` is not a capture
error; it is what live Python v2.1.2 emits for this data, and the port
reproduces it byte for byte. Whether Python *should* emit it is an upstream
question, not a parity one.

This is *not* the missing multi-site fixture: the report's per-site loop still
runs exactly once. See "Not yet covered by any fixture" below.

### How it was captured and blessed

```sh
# 1. the fixture -- an empty scratch directory first, so nothing is written
#    into the image tree
python3 AstroBinUpload.py "<scratch>/ic405" \
        "/mnt/raid0/AstroImaging/Preselected/Flaming Star Nebula (IC405) started 4th February 2022" \
        --debug --config parity/golden_config.ini
cp "<scratch>/ic405/AstroBinUploadInfo/debug_step_00_RawHeaders.csv" \
   parity/fixtures/ic405_raw.csv

# 2. the references -- a --test replay of that fixture, in a scratch directory
#    named for the original so the basename in the summary matches
python3 AstroBinUpload.py "<scratch>/Flaming Star Nebula (IC405) started 4th February 2022" \
        --test ".../raw.csv" --config parity/golden_config.ini
```

The replay reproduces the disk scan exactly: the acquisition CSVs are identical
and the summaries differ only in the `Generated` line and the embedded CSV name,
which is what the `ic405.basename` sidecar exists to control.

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

The port plan asked for "a tile-compressed `.fits.fz` case". Measured, that is
two different cases, and that phrasing picks the less useful one:

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

## Parity target bumped to v2.1.2 (2026-09-08)

Found while validating this port against the maintainer's own real,
unstructured data (not the corpus): every calibration section in the session
summary was labelled `MASTERxxx` unconditionally, even for a session built
entirely from raw `DARK`/`FLAT`/`BIAS` frames with no master anywhere. Fixed
upstream in Python `v2.1.2` (`engine/reports.py::format_image_type_table`)
and ported to `src/reports.rs` identically — see `CHANGELOG.md`'s `[2.1.2]`
entry in the Python repo for the full account, including the check against
the actual v1.4.7 source (still on disk in an old install) that the removed
code's own comment claimed to be following and wasn't.

`check_steps.py`'s `PARITY_TARGET` is now `"2.1.2"`. `sh2101_calib_summary.txt`
was re-copied from upstream (all-raw calibration data, so the three sections
that used to read `MASTERFLATS:`/`MASTERBIAS:`/`MASTERDARKS:` now correctly
read `FLATS:`/`BIAS:`/`DARKS:`); `sadr_summary.txt` was re-copied too but only
its `Generated` line changed (`sadr` carries no calibration frames at all).
`mosaic_summary.txt` — a local fixture, no upstream counterpart — was
re-blessed the same way, for the same reason (also all-raw). `xisf_mixed` and
`synthetic` in the binary corpus needed no change: neither is all-raw, so the
label was already `MASTERxxx` correctly before and after.

## Not yet covered by any fixture

Measured on 2026-09-09 by scanning the **whole** of `/mnt/raid0/AstroImaging`
with the port -- all 43,073 FITS and XISF files, header-only, 5m44s -- and
querying the resulting `debug_step_00_RawHeaders.csv`. An earlier pass that
sampled one FITS file per directory was not good enough: half the library is
XISF-only, and it produced a coordinate span (60 m) that the full census
contradicts (167.7 m). The negatives below are as much a result as the fixture
that closed `DARKFLAT`; each is a search that does not need repeating here.

- **`MASTERDARKFLATS`.** Raw `DARKFLAT` is covered by `ic405` as of 2026-09-09.
  A *master* darkflat is not, and cannot be: no file matching `*master*darkflat*`
  or `*master*flatdark*` exists anywhere in the library, and
  `Preselected/Calibration data/masters/` holds only `masterDark`, eight
  `masterFlat`s and a `superbias`.

- **A multi-site session**, meaning a run where `reports.py:361`'s
  `df.groupby(SITE_NAME)` loop iterates more than once. Two facts, both
  measured, and the second is the binding one:

  1. Multiple *clusters* are easy. 35,462 frames carry coordinates, in 71
     unique pairs, spanning 167.7 m -- more than `CLUSTER_RADIUS_M = 110.0`.
     Scanning the whole library produces **three** clusters:
     `(52.248430, -0.123145)`, `(0.0, 0.0)`, and `(52.247583, -0.124583)`.
  2. Clusters are not sites. The name comes from `_find_site_in_db`'s fuzzy
     match against `[sites]` in `golden_config.ini`, which holds exactly one
     reachable UK entry (`Norton Close, ...` at `52.2484, -0.1232`); every
     cluster that misses it falls back to the same `[defaults] SITE =
     Papworth Everard`. So all three clusters above carry one name, and the
     summary prints one `Site:` block.

  Two distinct names therefore needs one cluster matching the DB entry *and*
  another falling to the default. That was not reachable from any natural
  combination of directories: `LBN 548` alone matches the DB, but adding a
  second dataset shifts cluster 0's centroid off `-0.1232` (the centroid is
  the mean of the cluster's *unique* coordinate pairs), and adding a
  calibration-only tree adds nothing to it because `_align_coordinates` pulls
  those frames onto the nearest light. The third cluster above is **one single
  frame** in `Flaming Star Nebula Mosaic` -- a GPS outlier, not a second
  observing site. Hunting a directory combination that lands cluster 0 on the
  right side of the rounding boundary would be constructing the result, not
  finding it, so it was not done. This gap needs frames from a genuinely
  different observatory, or a second `[sites]` entry with data to match it.

- **A blank filter in a flat table.** Closed as unobtainable, decisively. Of
  17,482 flat-type frames in the library, **zero** lack a `FILTER` header, and
  no `'No Filter'` or `'None'` value appears anywhere: the nine distinct values
  are `Blue`, `CLS`, `Green`, `Ha`, `Lum`, `Lum (UV-IR Block)`, `OIII`, `Red`,
  `SII`. A missing `FILTER` occurs only on `DARK`, `BIAS`, `Master Dark` and
  `Master Bias` -- filter-independent types, where `base.py:231` supplies
  `'No Filter'` and `xisf_mixed` already covers the resulting blank rows. So
  what remains uncovered is only the blank-filter *flat*, and the data for it
  does not exist here.

- **An unmapped filter name.** Noticed while running the census above, not
  chased: `golden_config.ini`'s `[filters]` table maps eight names to AstroBin
  codes, but the library also contains `Lum (UV-IR Block)`, which is not one of
  them. No fixture carries a filter the config cannot map, so whatever the
  exporter emits for that case is unverified. Unlike the three gaps above, the
  data for this one *does* exist here -- it is simply not yet captured.

## Do not regenerate `fixtures/sadr_raw.csv`

It predates remediation A2 and therefore carries no `SOURCE_PATH` column,
which makes it the only fixture exercising the *degraded* filename-only
deduplication branch (and its warning). `sh2101_calib_raw.csv` exercises the
normal `(directory, basename)` path. Regenerating sadr would silently drop
half that coverage. See the port plan's hazard 13.

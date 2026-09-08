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

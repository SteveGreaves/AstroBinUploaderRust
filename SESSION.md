## 🎯 Objective

Port `AstroBinUpload.py` (a FITS/XISF metadata ETL that produces AstroBin bulk-upload
CSVs) to a self-contained Rust binary, reproducing the Python output **byte for byte**.

This repository is the port. It was split out of the Python project on 2026-09-07 at
the user's request — the two are now fully independent. The Python side is **complete
and released at v2.1.1**; no further Python work is expected.

Plan of record: **`PORT_PLAN.md`** — parity contract, dependency mapping, six phases,
and fourteen ranked parity hazards. Read it before writing code.

- This repo: `/mnt/raid0/Agent_Code/Astronomy/AstroBinUploaderRust` → github.com/SteveGreaves/AstroBinUploaderRust (**private**)
- Python upstream: `/mnt/raid0/Agent_Code/Astronomy/AstroBinUploader` → github.com/SteveGreaves/AstroBinUploader (public)
- Toolchain: cargo/rustc 1.98.1 at `~/.cargo/bin` (`. "$HOME/.cargo/env"`)
- Python venv for the parity scripts: `/mnt/raid0/Code/venvs/.astrovenv/bin/python3`

## ✅ Completed Work

**Phase 1 — complete** (`77c2eb9`): `src/cli.rs` (clap mirror of argparse), `src/config.rs`
(configobj-compatible parser), `src/table.rs` (CSV reader with pandas dtype inference,
rules *measured* against pandas 2.2.3). `parity/check_parity.py`: 3/3.

**Phase 2 — complete** (`514edfd`). All six pipeline steps are byte-identical to the
Python oracle on both fixtures: 465 lines (sadr) and 450 lines (sh2101_calib), every
column, every cell, column order and row order included. `python3 parity/check_steps.py`.

**Phase 4 — complete, 2026-09-08.** The FITS and XISF readers. The binary now runs
from **real files on disk**: `astrobin-upload <dir> --config <ini>` scans, and a scan of
`parity/fixtures/binary/Sadr Region` (221 real N.I.N.A. FITS) produces both artifacts
byte-identical to the committed `sadr` reference — the same reference the CSV fixture
uses, so the reader was checked against a target that existed before it did. Verify with
`python3 parity/check_readers.py`: 3/3 scenarios, every pipeline step cell-identical and
both artifacts matching.

- `src/pathutil.rs` — `os.path` POSIX semantics. `abspath` normalises the string and
  leaves symlinks alone, unlike `canonicalize`; `SOURCE_PATH` is an abspath and its
  `dirname` is half of the dedup key, so the difference reaches output.
- `src/extractor.rs` — traversal (hazard 8), the quote-strip, and `Table::from_records`.
- `src/fits.rs` — hand-written header reader, no cfitsio. HDU selection, card typing,
  and astropy's compressed-header translation.
- `src/xisf.rs` — the XML header block and its four fallbacks.
- `parity/check_readers.py` — the Phase 4 harness.

**Three things worth keeping:**

1. **`pd.DataFrame(list_of_dicts)` inference is not `read_csv` inference**, and the
   scan path uses the first. Measured table in `Table::from_records`' doc comment. The
   one that bites: `"None"` stays the literal string on a scan, where `read_csv` makes
   it NA — so `[defaults]`' `None` survives a scan but not a `--test` replay. Also
   `bool` + a missing key → `object`, and the scan path never upper-cases column names.
2. **astropy synthesises a header for a compressed HDU** rather than reporting the
   BINTABLE on disk: `XTENSION` becomes `IMAGE`, the `Z`-prefixed cards replace the
   table's geometry, and the whole compression machinery — `EXTNAME` included —
   disappears. The frame's columns are whatever astropy returned, so `fits.rs` performs
   the same substitution. None of it reaches the artifacts; all of it decides the raw
   frame's column set.
3. **Column order on a scan is first appearance across every file scanned**, so one
   unexpected card in the first file shifts every column after it. Both dumps now emit
   a `COLS` line per step so that failure is one short diff rather than sixty shifted
   ones.

**Phase 3 — complete, 2026-09-08.** The exporter and `reports.py`. Both artifacts —
`<basename>_acquisition.csv` and `<basename>_session_summary.txt` — are byte-identical
to the committed references on both fixtures, and the statistics the summary rounds
away are bit-identical. Verify with `python3 parity/check_reports.py` (needs
`cargo build` first). The binary now runs end to end: `astrobin-upload <dir> --test
<csv> --config <ini>` writes both files into `<dir>/AstroBinUploadInfo/`.

- `src/pandas_fmt.rs` — `DataFrame.to_string(index=False)`, hazard 1.
- `src/exporter.rs` — the LIGHT-only acquisition frame, `to_csv`, file writing.
- `src/reports.rs` — `generate_full_summary` and its five helpers.
- `parity/check_reports.py` — replays each fixture in `run_golden.py`'s scenario-dir
  layout, byte-compares both artifacts (normalising only `^Generated .*$`, exactly as
  upstream does), then compares the temperature statistics as raw IEEE-754 bits.

**Three findings worth keeping, all measured, all now in `PORT_PLAN.md`:**

1. **Hazard 1's extra space comes from the header, not the values.** With
   `index=False` pandas passes `leading_space=False` to every array formatter, so no
   value gets a prefix; `_get_formatted_column_labels` prepends `" "` to the *name* of
   every `is_numeric_dtype` column. Column width is then
   `max(len(header), max(len(value)))`, right-justified, columns adjoined by a single
   space. The plan's original explanation was wrong and is corrected in place.
2. **The per-column decimal count is a trim loop, not a round-trip search.** Every
   value is rendered `%.6f`, then `_trim_zeros_float` strips one trailing character
   from every decimal-looking entry for as long as they *all* still end in `0`, then
   restores one `0` after a bare decimal point.
3. **The bare `Series.mean()` hazard 4 warned about exists**, at exactly one call site:
   "Mean temperature". It is numpy **pairwise** summation over the NA-as-zero values
   (`steps::pairwise_sum`), and the corpus can actually discriminate it — a left fold
   matches `sadr` but not `sh2101_calib`, Kahan matches `sh2101_calib` but not `sadr`,
   pairwise matches both.

**Also true and easy to forget:** the CSV rounding is `DataFrame.round()`, i.e.
`numeric::numpy_round`, *not* the builtin `python_round` that `optical.py` uses. Both
are live in this codebase (hazard 3) and `exporter::tests` pins the distinction.

## 🚧 Current Blockers & Technical Debt

- **Nothing blocking.** Builds clean with no warnings, 123 tests pass, all four parity
  harnesses green (`check_parity.py` 5/5, `check_steps.py` 4/4, `check_reports.py` 4/4,
  `check_readers.py` 3/3).
- Dependencies: **`chrono`** (`clock` feature only) for the `Generated <timestamp>` line
  in local time — chosen over `libc::localtime_r` because Phase 5's release matrix
  includes Windows, where that function does not exist — and **`roxmltree`** for the
  XISF XML block. The FITS reader is hand-written and depends on nothing.
- `fits.rs` does **not** implement `CONTINUE` long-string cards or `HIERARCH` keywords.
  Measured across all 227 FITS files in the corpus, the only non-value cards are `END`,
  blank padding, `HISTORY` and `COMMENT`. An undefined-value card (`KEY = / comment`)
  becomes an empty string where astropy carries an `Undefined` sentinel; no card in the
  corpus is undefined, and that is the one place a divergence could hide.
- `check_steps.py` and `check_reports.py` verify their own oracle before running: the
  sibling Python checkout must be at v2.1.1 and the committed corpus copies must still
  match its `golden_tests/`. `ASTROBIN_PY_REPO` / `ASTROBIN_PYTHON` override the paths.
- Paths deliberately narrow because no fixture reaches them, each saying so in code:
  - `datetime.rs` / `normalize.rs::parse_iso_datetime` handle ISO-8601 only.
  - `steps::promote` resolves only same-dtype and int/float mixes.
  - `pandas_fmt.rs`'s scientific-notation and empty-frame branches are transcribed but
    unexercised by the corpus.
  - `reports.rs`'s `'No Filter'`/`'None'` → blank substitution in the flat tables is
    unexercised: every MASTERFLAT row in `sh2101_calib` carries a real filter.
- **The corpus is now four fixtures, every one replayable** (2026-09-08). `mosaic`
  (1544 rows, four-panel mosaic, all three master tables) and `lbn548` (270 rows,
  lights only, no calibration at all) were captured from the user's data and blessed
  against live v2.1.1; the four unverifiable orphan references were deleted.
  `mosaic_summary.txt` was re-blessed, not removed — the committed copy predated
  remediation A14 and showed `Ha` on MASTERDARKS/MASTERBIAS rows where v2.1.1 leaves
  the filter blank. `parity/CORPUS.md` records all of it, including which half of the
  corpus is an upstream copy (`check_steps.py::UPSTREAM_FIXTURES`, staleness-checked)
  and which is local (`LOCAL_FIXTURES`, deliberately not).
- Still uncovered by any fixture: **`DARKFLAT`/`MASTERDARKFLATS`** (200 frames exist at
  `Preselected/Calibration data/24th February 2022/FlatWizard/`, but they need a
  same-era lights set to pair with), a **multi-site session** — the site loop has never
  run twice — and a **blank filter in a flat table**.
- `src/pathutil.rs` is POSIX-only by design — it is a transcription of `posixpath`, and
  every expectation in its tests was checked against the real module. On Windows, Python
  would use `ntpath`, which splits on `\` and the drive letter too, so paths would be
  handled differently under the two implementations. Harmless until Phase 5 ships
  Windows binaries; fix it there, in one file.
- **The binary corpus now exists** at `parity/fixtures/binary/` (241 files, 2.4 MiB of
  content), built by the committed `parity/make_binary_fixtures.py` and
  `parity/make_synthetic_fits.py`. Every real file is a header-only truncation, which
  is lossless here — the pipeline never reads pixel data. Three scenarios:
  `Sadr Region/` (221 FITS; **a live-Python scan of it reproduces the committed
  `sadr` reference byte for byte**, so Phase 4's FITS reader has an end-to-end target
  already in the repo), `xisf_mixed/` (14 files: lights, one raw frame per calibration
  type, three PixInsight masters, WBPP `_c_lps_r` names), and `synthetic/` (6 generated
  FITS isolating each HDU-selection rule). References for the latter two are
  `parity/references/binary_*`, blessed from a live v2.1.1 **disk scan**.
  `make_binary_fixtures.py --check` verifies the corpus without the source images.
- **`PORT_PLAN.md`'s "tile-compressed `.fits.fz` case" is two cases, and the plan names
  the less useful one.** The traversal filter is `('.fits', '.fit', '.fts', '.xisf')`
  (`extractor.py:88`), so a file actually *named* `.fits.fz` is silently skipped and
  never reaches the reader. A7's bug needs a compressed image inside a file named
  `.fits`. `synthetic/01_compressed.fits` and `06_compressed.fits.fz` are byte-identical
  and pin both halves.
- Upstream tracker is clear; issues #9 and #10 remain open pending a re-test.
- The user is **content** that their home address appears in the public upstream repo's
  golden references. Do not raise it again.

## 🚀 Next Steps

**Phase 5: `rayon` parallelism and the release matrix.** The port is now functionally
complete — every phase of the pipeline, from a directory of FITS/XISF files to both
artifacts, is byte-identical to Python v2.1.1. What is left is speed and packaging.

Two things to know before starting Phase 5:

- **`extract_from_directories` is the only place worth parallelising**, and it is
  already shaped for it: `scan_directories` produces the sorted path list, then each
  file is read independently. The Python uses a `ProcessPoolExecutor` and reassembles
  results *in the sorted dispatch order* rather than completion order; a `rayon`
  `par_iter().map(...)` over the sorted list preserves order by construction, so the
  hazard the Python had to work around does not arise. Row order is the whole ballgame
  (hazard 8) — `check_readers.py` catches any regression.
- **`pathutil.rs` is POSIX-only**, and the Windows targets in the release matrix need
  `ntpath` semantics. That is the one file to fix.

Then Phase 6 (differential harness in CI).

The method that carried Phases 2 to 4 transfers again:

1. **Get the oracle emitting the target before writing any logic**, then diff.
2. **Add one unit at a time and diff after each**, so every diff has one candidate cause.
3. **Diff strictly**, byte for byte with no line stripping — `reports.py`'s `{:<15}`
   padding leaves trailing spaces that a normalising diff would hide.
4. **Where the artifact rounds a value away, compare the value too.** The
   `--dump-report-stats` flag exists because a green summary diff says nothing about
   how a mean was summed; without it the pairwise transcription would be unverified.
5. **Measure the library, do not reason about it.** Every surprise in Phases 3 and 4 —
   the `to_string` header space, the trim loop, `pd.DataFrame(records)` inference,
   astropy's compressed-header synthesis, the `.fz` exclusion — came from running the
   Python and reading the answer, and several contradicted what the plan assumed.

## 📂 Files to Load

- `PORT_PLAN.md` — plan of record. Phase 5 is the release matrix and `rayon`.
- The four harnesses — run all of them first to confirm the starting state is green:
  `check_parity.py`, `check_steps.py`, `check_reports.py`, `check_readers.py`.
- `src/extractor.rs` — `scan_directories` is the seam Phase 5 parallelises.
- `src/pathutil.rs` — POSIX-only; the Windows targets need `ntpath` semantics.
- `Cargo.toml` — the release profile the matrix builds on.

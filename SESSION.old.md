## 🎯 Objective

Port `AstroBinUpload.py` (a FITS/XISF metadata ETL that produces AstroBin bulk-upload
CSVs) to a self-contained Rust binary, reproducing the Python output **byte for byte**.

This repository is the port. It was split out of the Python project on 2026-09-07 at
the user's request — the two are now fully independent. The Python side is **complete
and released at v2.1.2**; no further Python work is expected unless another real-data
surprise turns one up, as v2.1.2 itself did.

Plan of record: **`PORT_PLAN.md`** — parity contract, dependency mapping, six phases,
and fourteen ranked parity hazards. Read it before writing code.

- This repo: `/mnt/raid0/Agent_Code/Astronomy/AstroBinUploaderRust` → github.com/SteveGreaves/AstroBinUploaderRust (**private**)
- Python upstream: `/mnt/raid0/Agent_Code/Astronomy/AstroBinUploader` → github.com/SteveGreaves/AstroBinUploader (public)
- Toolchain: cargo/rustc 1.98.1 at `~/.cargo/bin` (`. "$HOME/.cargo/env"`)
- Python venv for the parity scripts: `/mnt/raid0/Code/venvs/.astrovenv/bin/python3`

## ✅ Completed Work

**Phase 6 — complete, 2026-09-08.** `.github/workflows/differential-harness.yml`: all four
`check_*.py` scripts run on every push to `main`, checking out the public Python oracle
as a sibling directory (no maintainer-machine paths) and byte-comparing it against this
port on GitHub's own runners. Report-only (`continue-on-error` per step) per
`PORT_PLAN.md` decision 4 — this repo has no PR workflow to gate yet, and the harness is
meant to retire after one release of overlap, not become permanent infrastructure.
**Run for real, not just authored and trusted**: pushed, watched with `gh run watch`,
confirmed every one of the sixteen individual fixture/scenario checks actually passed
(not just green step icons) — `check_parity.py` 5/5, `check_steps.py` 4/4,
`check_reports.py` 4/4, `check_readers.py` 3/3, 1m30s total. Every phase in
`PORT_PLAN.md`'s plan of record is now done.

**Post-Phase-5 fix, 2026-09-08 — parity target bumped to v2.1.2.** Running the port against
the user's real, unstructured PixInsight/calibration directories (not the corpus) surfaced a
real bug: every calibration section in the session summary read `MASTERxxx` unconditionally,
even for a session built entirely from raw `DARK`/`FLAT`/`BIAS` frames. Traced to Python, not
Rust — confirmed with the live oracle on the user's exact data before touching any code — then
traced *inside* Python to a comment that claimed "v1.4.7 standards" for behaviour the actual
v1.4.7 source (still on disk in an old install) never had: v1.4.7 labelled each section by its
literal `IMAGETYP`, plain `DARK:` for raw frames, `MASTERDARK:` only for genuine masters.

Fixed upstream first (Python `v2.1.2`, `engine/reports.py::format_image_type_table`, both
golden fixtures re-blessed, `pytest` green), then ported the identical fix to `src/reports.rs`.
`PARITY_TARGET` in `check_steps.py` and every version pin in this repo (`Cargo.toml`, this
file, `README.md`, `PORT_PLAN.md`, `parity/CORPUS.md`) now say `2.1.2`. `parity/CORPUS.md` has
the full account of what was re-blessed and why. Verified on the user's exact real command
after the fix.

**Phase 1 — complete** (`77c2eb9`): `src/cli.rs` (clap mirror of argparse), `src/config.rs`
(configobj-compatible parser), `src/table.rs` (CSV reader with pandas dtype inference,
rules *measured* against pandas 2.2.3). `parity/check_parity.py`: 3/3.

**Phase 2 — complete** (`514edfd`). All six pipeline steps are byte-identical to the
Python oracle on both fixtures: 465 lines (sadr) and 450 lines (sh2101_calib), every
column, every cell, column order and row order included. `python3 parity/check_steps.py`.

**Phase 5 — complete, 2026-09-08.** `rayon` parallelism and the five-target release matrix,
both verified rather than assumed.

- `extract_from_directories` now reads files in parallel. Row order (hazard 8) comes from
  `par_iter().map().collect()` into an indexed `Vec`, which preserves input order by
  construction — no reassembly step exists to get wrong, unlike the Python's
  `ProcessPoolExecutor` + explicit re-sort. Measured on this project's own rotational
  disk (an `ST8000DM004`; the astro datasets are not on SSD), same code path both times
  via `RAYON_NUM_THREADS=1`: **15.0 ms/file serial vs 6.6 ms/file parallel — 2.3x**. The
  win is hiding seek latency, not CPU time. Correctness unaffected: `check_readers.py`
  still cell-identical, and three repeated parallel scans of the 221-file `Sadr Region`
  tree hash identically.
- `src/pathutil.rs` is restructured into `posix` and `windows` submodules — mirroring how
  CPython itself keeps `posixpath` and `ntpath` as plain, always-importable modules and
  lets `os.path` alias one at runtime. Here the choice is `#[cfg(windows)]`, made at
  compile time, and **both modules stay always compiled and always tested** on every
  host — which is what let `cargo test` on this Linux machine catch a real bug in the
  Windows-target logic before it ever reached a Windows machine (see below).
- `.github/workflows/release-matrix.yml` — five targets from `PORT_PLAN.md` decision 2:
  `x86_64-unknown-linux-musl` (static), `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`,
  `x86_64-apple-darwin`, `aarch64-apple-darwin`. **Actually run on GitHub's real runners,
  not just written and assumed correct** — `gh workflow run` + `gh run watch`, three times,
  fixing what each run found:
  1. Both Windows legs failed at `git checkout`, before any Rust code ran: a committed
     fixture path (`parity/fixtures/binary/xisf_mixed/registered/Light_BIN-1_.../Sh2 101_..._c_lps_r.xisf`,
     a real WBPP-generated name) exceeds Windows' 260-character `MAX_PATH`. Fixed with
     `git config --global core.longpaths true` before checkout — a repo/CI config issue,
     not a code defect, and not worth shortening a fixture name that is itself evidence
     (it's what the dedup regex is tested against).
  2. With checkout fixed, `x86_64-pc-windows-msvc`'s `cargo test` failed on exactly one
     test: `posix::abspath("rel").starts_with('/')`. `abspath` calls
     `std::env::current_dir()`, and on that runner the real cwd is a Windows path with no
     leading `/` — the assumption was correct on every host this code had run on so far,
     and wrong on the first one that wasn't POSIX. Fixed by dependency injection:
     `abspath_with_cwd(p, cwd)` takes the current directory as a parameter, and both
     submodules' tests now supply a known synthetic cwd instead of reading the live one —
     restoring the "tested identically on any host" property the module doc had claimed
     before it was actually true.
  3. **All five legs green** on the third run: `x86_64-unknown-linux-musl` and
     `aarch64-apple-darwin` run the full test suite (132 tests) on their native
     architecture; `x86_64-pc-windows-msvc` likewise; `aarch64-pc-windows-msvc` and
     `x86_64-apple-darwin` cross-compile and build-verify only, since the runner's own
     CPU cannot execute either result. Five binaries uploaded, 594–786 KB each.

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
- Both readers are **incremental**, and that is not decorative: `fits.rs` seeks past
  every data unit and `xisf.rs` reads exactly the declared XML length. Slurping the
  file would behave identically on the truncated fixtures — smallness is precisely what
  hides it — and move 27 GB over the real `Sadr Region` tree, multiplied by core count
  once Phase 5 adds `rayon`. Verified on untruncated originals: 856 MB of real files
  (a 122 MB FITS and a 734 MB PixInsight master) scan in 10 ms at 6.4 MB peak RSS, and
  still dump identically to Python.
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

**All six phases of `PORT_PLAN.md` are complete.** The port is functionally done and
verified: byte-identical to Python v2.1.2 end to end, building and passing its own tests
on five release targets, with a live differential harness confirming it on every push.
There is no mandated next phase — what follows is maintenance-shaped work, not a plan
item:

- **Retire the differential harness eventually**, per decision 4 — it was scoped to run
  "during development and for one release of overlap, then retire," not stay forever.
  Not yet: there has been no release to overlap with.
- **Fixture gaps still on record** (see Blockers above): `DARKFLAT`/`MASTERDARKFLATS`,
  a multi-site session, a blank filter in a flat table. Each needs a matching real
  dataset before it can be closed the way this session closed everything else — by
  measuring, not guessing.
- **Windows/macOS binaries are built and tested in CI but never run by a human** on
  either platform. The first real use on Windows is also the first real test of
  `pathutil::windows` outside its unit tests and of `SITELAT`/`SITELONG` fallback paths
  the corpus doesn't reach.
- **A real release** — tagging a version, publishing the five binaries somewhere a user
  would actually get them from — has not happened. Everything so far has been `cargo
  build` and CI artifacts.

The method that carried Phases 2 to 4 transfers again:

1. **Get the oracle emitting the target before writing any logic**, then diff.
2. **Add one unit at a time and diff after each**, so every diff has one candidate cause.
3. **Diff strictly**, byte for byte with no line stripping — `reports.py`'s `{:<15}`
   padding leaves trailing spaces that a normalising diff would hide.
4. **Where the artifact rounds a value away, compare the value too.** The
   `--dump-report-stats` flag exists because a green summary diff says nothing about
   how a mean was summed; without it the pairwise transcription would be unverified.
5. **Measure the library, do not reason about it.** Every surprise in Phases 3–5 — the
   `to_string` header space, the trim loop, `pd.DataFrame(records)` inference, astropy's
   compressed-header synthesis, the `.fz` exclusion, the Windows long-path failure, the
   `abspath` test that only broke on a non-POSIX host — came from actually running
   something (Python, or a real CI runner) and reading the answer. Phase 5 in particular
   is the clearest case yet: writing the release-matrix YAML and reasoning about it would
   have shipped both bugs undetected. Running it three times on real runners, fixing what
   each run found, is what actually verified it.

## 📂 Files to Load

- `PORT_PLAN.md` — plan of record. All six phases done; read the "Not yet covered by
  any fixture" gaps in `parity/CORPUS.md` before assuming there's nothing left.
- The four harnesses — run all of them first to confirm the starting state is green:
  `check_parity.py`, `check_steps.py`, `check_reports.py`, `check_readers.py`. They also
  now run automatically on every push (`.github/workflows/differential-harness.yml`).
- `.github/workflows/release-matrix.yml` and `differential-harness.yml` — the pattern any
  future workflow here should follow: real verification via `gh run watch`, not just
  written and trusted.
- `PORT_PLAN.md`'s decision 4, on how much CI infrastructure the differential harness is
  meant to become before it retires.

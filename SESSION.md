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
(configobj-compatible parser: bracket-count nesting, quoted section names, no type
coercion, comma-implies-list), `src/table.rs` (CSV reader with pandas dtype inference,
rules *measured* against pandas 2.2.3). `parity/check_parity.py` diffs all of it against
live configobj and pandas: 3/3.

**Phase 2 — complete, 2026-09-08.** All six pipeline steps are byte-identical to the
Python oracle on both fixtures: **465 lines (sadr) and 450 lines (sh2101_calib), every
column, every cell, column order and row order included.** Verify with
`python3 parity/check_steps.py` (needs `cargo build` first).

- `src/dump.rs` + `--dump-steps` — the Rust half of the per-step oracle, emitting the
  exact byte sequence `parity/dump_steps.py` produces. Columns go out in **frame order,
  never sorted**; a sorted dump cannot see a column-ordering bug, and column order is
  load-bearing here.
- `src/appconfig.rs`, `src/constants.rs`, `src/datetime.rs`, `src/steps/*` — the typed
  config layer and the six steps as pure `Table -> Table` functions.
- `parity/check_steps.py` — runs both dumps and prefix-diffs them. `parity/narrow.py`
  localises a mismatch to individual rows (a column is one line of up to 1693 values, so
  plain `diff` names the column and nothing else).

**Four findings that changed the plan, all measured, all now in `PORT_PLAN.md`:**

1. **pandas' CSV float converter is not correctly rounded and Rust's is** (hazard 14b).
   `read_csv`'s default `float_precision=None` selects `precise_xstrtod`: 17 significant
   digits into a double, then one tabulated power-of-ten scaling. 62 of 221 `FWHM` values
   in `sadr_raw.csv` differed by an ULP. Transcribed in `table.rs::precise_xstrtod`.
   `pd.to_numeric` shares that converter; a bare `float()` in Python source does **not**
   (`steps::to_numeric` vs `steps::python_float` — the distinction is per call site).
2. **Grouped `mean` is Kahan-compensated**, in row order — not the pairwise summation of
   `Series.mean()`/`np.mean()` (hazard 4). Live in the GPS cluster centroids and in nine
   aggregation columns.
3. **Both rounding algorithms are live** (hazard 3): builtin `round` for HFR/IMSCALE/FWHM,
   numpy's for `exposure`/`gain`, and one of each on either side of the site-lookup
   equality in `geocode.rs`.
4. **The `--debug` per-step CSVs are a lossy oracle — do not use them.** `to_csv` →
   `read_csv` destroys exactly what this port must get right. Use `parity/dump_steps.py`.

**Data-model decision** (now written into `PORT_PLAN.md`'s data-layer section): the
dynamic column-oriented `Table`, not the plan's original `Vec<Frame>`. `[defaults]`,
`[override]` and `[equipmentoverrides]` let a user *create* arbitrary columns, so the
frame's shape is user-defined. Every row operation — filter, `concat`, `groupby`
reordering — reduces to `Table::take_rows(&[usize])`, which keeps row order explicit.

## 🚧 Current Blockers & Technical Debt

- **Nothing blocking.** Builds clean with no warnings, 83 tests pass, both parity harnesses
  green. Phase 2 is pushed; local `main` and `origin/main` are in sync at `514edfd`.
- `check_steps.py` verifies its own oracle before running: the sibling Python checkout must
  be at the parity target (v2.1.1 per `_version.py`) and the committed corpus copies must
  still match that checkout's `golden_tests/`. `ASTROBIN_PY_REPO` and `ASTROBIN_PYTHON`
  override the paths.
- Two paths are deliberately narrow because no fixture reaches them, and both say so in
  code rather than pretending otherwise:
  - `normalize.rs::parse_iso_datetime` / `datetime.rs::parse` handle ISO-8601 only. The
    master-preference DATE-OBS tie-break needs two masters in one hardware group; neither
    fixture has that.
  - `steps::promote` resolves only same-dtype and int/float mixes. Anything mixing a
    string with a number would be an `object` column in pandas, and rendering numbers into
    one means reproducing Python's `repr` — worth doing when a fixture needs it.
- `parity/` holds **copies** of the upstream golden corpus (`parity/CORPUS.md` records
  provenance: `AstroBinUploader@d2f61bc`, `v2.1.1`). If the Python side ever re-blesses its
  references, re-copy and update that file.
- **`golden_tests/fixtures/binary/` does not exist upstream.** Phase 4 must build it first,
  including a tile-compressed `.fits.fz` case.
- Upstream tracker is clear; issues #9 and #10 remain open pending a re-test against the
  reporter's data.
- The user is **content** that their home address appears in the public upstream repo's
  golden references. Do not raise it again.

## 🚀 Next Steps

**Phase 3: the exporter and `reports.py` — the byte-parity grind, and where hazard 1
lives** (`acq_df.to_string()` appended to the summary: pandas' own column-width and
float-repr rules, reproduced exactly). Read hazards 1, 5 and 6 in `PORT_PLAN.md` first.

The method that worked for Phase 2 transfers directly, and is worth repeating:

1. **Get the oracle emitting the target before writing any logic**, then diff. Doing
   `00_raw` alone first is what isolated the `precise_xstrtod` finding instead of leaving
   it tangled inside stage 7.
2. **Add one unit at a time and diff after each.** Every step landed first-try after the
   float fix, because each diff had exactly one candidate cause.
3. **Diff strictly** (line-for-line over the emitted prefix), not just per-column — that is
   what proves column order.
4. Phase 3 has no per-step dump to lean on: the oracle is the committed
   `parity/references/*_summary.txt` and `*_acquisition.csv`. Start by making the binary
   produce the acquisition CSV from the aggregated frame and diffing against
   `parity/references/sadr_acquisition.csv`; the summary, with its `to_string()` table, is
   the harder half and should come second.

Then Phase 4 (FITS/XISF readers, needs the binary fixtures built), 5 (rayon, release
matrix), 6 (differential harness in CI).

## 📂 Files to Load

- `PORT_PLAN.md` — plan of record. Phase 3 touches hazards 1, 5, 6.
- `parity/check_steps.py` — run first to confirm the starting state is green.
- `src/steps/aggregate.rs` — produces the frame Phase 3 formats; its rules list is the
  column order of the export.
- `src/dump.rs` — the canonical form, and why nothing in it is sorted.
- Upstream, read alongside: `../AstroBinUploader/engine/exporter.py` and
  `engine/reports.py`.

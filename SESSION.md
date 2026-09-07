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
- Toolchain: cargo/rustc 1.98.1, installed this session at `~/.cargo/bin` (`. "$HOME/.cargo/env"`)
- Python venv for the parity scripts: `/mnt/raid0/Code/venvs/.astrovenv/bin/python3`

## ✅ Completed Work

**Phase 1 — complete and verified** (commit `77c2eb9`)

- `src/cli.rs` — clap mirror of the Python argparse surface (same flags, defaults, arity),
  including the exact invocation the golden harness uses.
- `src/config.rs` — configobj-compatible parser. Nests by bracket count, unquotes section
  names so a site name keeps its commas, and reproduces the two behaviours that silently
  break `[override]` matching if missed: **no type coercion** (`0` is the string `"0"`)
  and **comma-implies-list**.
- `src/table.rs` — CSV reader with pandas dtype inference. Rules were *measured* against
  pandas 2.2.3, not assumed. Several were not in the plan's first draft: `'None'` is an NA
  sentinel, `True`/`False` infer `bool`, leading zeros are lost to `int64`, and an integer
  past i64 stays `object` rather than falling back to float.
- `src/main.rs` — Phase 1 entry point plus a hidden `--dump-parity` mode. Exits non-zero
  saying the pipeline is unimplemented rather than pretending otherwise.
- `parity/check_parity.py` — differential check against **live** configobj and pandas.
  Currently: config 59 lines, sadr 124, sh2101_calib 119 — all byte-identical.

**Phase 2 groundwork** (commits `585446b`, `8133b1a`)

- `parity/dump_steps.py` — the per-step oracle. Walks the Python pipeline in-process and
  emits each step's frame as canonical `STEP/name/column/dtype/values` lines, floats as raw
  IEEE-754 hex bits.
- `src/numeric.rs` — `python_round` (CPython builtin, decimal ties-to-even) **and**
  `numpy_round` (multiply–rint–divide on the binary double). Verified against the five
  boundary values in the upstream `REMEDIATION_PLAN.md` A12.
- `PORT_PLAN.md` — hazard 3 extended with the four-call-site rounding table (below).

**Two findings that changed the plan, both verified not assumed:**

1. **The `--debug` per-step CSVs are a lossy oracle — do not use them.** `to_csv` →
   `read_csv` destroys exactly what this port must get right. Measured on `debug_step_01`
   for sadr: `rotname` is `object`/`'None'` in memory but `float64`/all-NaN through the CSV;
   `useobsdate` is `object`/`'False'` in memory but `bool` through the CSV. Both sides of
   such a comparison end up equally corrupted — false agreement, undetectable. Use
   `parity/dump_steps.py`.
2. **Both rounding algorithms are live.** The plan originally documented only the builtin.

   | Call site | Call | Algorithm |
   |---|---|---|
   | `optical.py::_python_round` (HFR, IMSCALE, FWHM) | `round(x, 2)` | builtin |
   | `base.py` Stage 7, `exposure` | `.round(2)` | pandas/numpy |
   | `base.py` Stage 7, `gain` | `.round()` then `astype(int)` | pandas/numpy |
   | `geocode.py::_find_site_in_db` | `db_lat.round(p)` vs `round(lat, p)` | **one of each**, either side of one equality |

**Data-model decision (deviates from `PORT_PLAN.md`):** use the dynamic column-oriented
`Table`, not the plan's suggested `Vec<Frame>` struct. Reason: `[defaults]`, `[override]`
and `[equipmentoverrides]` let a user *create* arbitrary columns from config, so the carrier
must tolerate arbitrary names. (Not because 72 columns survive step 1 — those passthrough
columns die at aggregation and never reach the output.) Add typed accessors for known
columns so step logic isn't stringly-typed. **This deviation is not yet written into
`PORT_PLAN.md`'s data-layer section — do that.**

## 🚧 Current Blockers & Technical Debt

- **Nothing blocking.** Builds clean, 23 tests pass, parity checks pass.
- `parity/` holds **copies** of the upstream golden corpus so this repo tests standalone.
  `parity/CORPUS.md` records provenance (`AstroBinUploader@d2f61bc`, `v2.1.1`). If the
  Python side ever re-blesses its references, re-copy and update that file.
- **`golden_tests/fixtures/binary/` does not exist upstream.** `PORT_PLAN.md` Phase 4 says
  to validate FITS/XISF readers against synthetic binary fixtures from the upstream P0 plan;
  those were never built. Phase 4 must build them first, including a tile-compressed
  `.fits.fz` case.
- **Upstream PR #13 is still open and unmerged** (a one-line `SESSION.md` doc change on the
  Python repo). `gh pr merge` and `gh issue close` were denied by this session's auto-mode
  classifier all session. Adding `"Bash(gh:*)"` to `autoMode.allow` in
  `~/.claude/settings.json` fixes it; the user has not done so. Several upstream issues are
  fixed in code but still show open for the same reason — see the reconciliation table at
  the end of the upstream `REMEDIATION_PLAN.md`.
- The user is **content** that their home address appears in the public upstream repo's
  golden references (it is the reverse-geocoded FITS GPS). Do not raise it again.

## 🚀 Next Steps

Phase 2: the six pipeline steps as pure functions. Split into **two commits** — steps 1–3,
then steps 4–6 — rather than one large one.

1. **Dump both fixtures first.** `sadr` does **not** exercise steps 3 or 4 (dedup drops
   nothing; it has zero calibration frames, so step 4 just adds four zero columns and
   matches trivially). Only `sh2101_calib` tests those meaningfully:
   ```
   python3 parity/dump_steps.py parity/fixtures/sadr_raw.csv         parity/golden_config.ini > /tmp/py_sadr.txt
   python3 parity/dump_steps.py parity/fixtures/sh2101_calib_raw.csv parity/golden_config.ini > /tmp/py_sh2101.txt
   ```
2. **Build the Rust side of the oracle**: a typed `AppConfig` (defaults / overrides /
   equipment_overrides / filters / sites / use_obs_date), `Table` mutation ops
   (insert, drop, rename, coalesce-duplicates, row-mask select, concat), and a
   `--dump-steps` flag emitting the identical canonical form. Then diff.
3. **Implement `NormalizeHeadersStep`** — the largest single step, seven stages. All
   semantics have been read; the ones easy to get wrong:
   - Stage 2 lowercases column names, then coalesces duplicates — and the coalesce
     **sorts the columns**, but *only when duplicates exist*. No duplicates ⇒ original order.
   - Stage 3 injects a default only if the lowercased key is still absent (A8 ordering).
   - Stage 3b applies `[equipmentoverrides]` **after** defaults, overwriting the whole column.
   - Stage 5 (master preference) recombines as `concat([lights, cals], ignore_index=True)`,
     which **reorders rows** — all lights first, then calibration frames.
   - Stage 6 applies the IMAGETYP keyword map **longest-keyword-first** against a *frozen*
     copy of the column, with an `assigned` mask so each row is rewritten once (this is A13).
   - Stage 7 hardening is asymmetric: a float list via `astype(float)`, `exposure` via
     pandas `.round(2)`, `gain` via pandas `.round()` + `astype(int)`, `number` via
     `fillna(1).astype(int)`, `site` via `astype(str).replace('nan', default)`.
4. Then `OpticalParameterStep` (uses `python_round`) and `DeduplicateStep` (anchored WBPP
   regex; key is `(dirname(source_path), base_filename)` with a documented fallback when
   `SOURCE_PATH` is absent — **sadr exercises that fallback, sh2101_calib the normal path**).
5. Record the `Table`-over-`Vec<Frame>` deviation in `PORT_PLAN.md`'s data-layer section.

## 📂 Files to Load

- `PORT_PLAN.md` — plan of record. Hazards 2, 3, 10–14 are the ones Phase 2 touches.
- `parity/dump_steps.py` — the oracle, and its docstring explains why the `--debug` CSVs
  cannot be used.
- `src/table.rs` — the data carrier Phase 2 extends.
- `src/numeric.rs` — the two rounding helpers and which call sites use which.
- `src/config.rs` — parsed config; Phase 2 needs the typed `AppConfig` layered on top.
- Upstream, read alongside: `../AstroBinUploader/engine/steps/base.py` (all seven stages),
  then `optical.py`, `deduplicate.py`, `calibration.py` (lines 40–125: `create_hybrid_key`
  gain handshake and `is_orphaned`), `geocode.py`, `aggregate.py`.

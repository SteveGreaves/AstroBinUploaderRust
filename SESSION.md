## 🎯 Objective

Port `AstroBinUpload.py` (a FITS/XISF metadata ETL that produces AstroBin bulk-upload
CSVs) to a self-contained Rust binary, reproducing the Python output **byte for byte**.

**All six phases of `PORT_PLAN.md` are complete.** This session finished Phases 5 and 6,
then moved into real-world validation, which surfaced two genuine bugs (one in Python,
not the port), and closed out the upstream issue tracker.

- This repo: `/mnt/raid0/Agent_Code/Astronomy/AstroBinUploaderRust` → github.com/SteveGreaves/AstroBinUploaderRust (**private**)
- Python upstream: `/mnt/raid0/Agent_Code/Astronomy/AstroBinUploader` → github.com/SteveGreaves/AstroBinUploader (public)
- **Parity target: Python `v2.1.2`** (bumped from 2.1.1 this session — see below)
- Toolchain: cargo/rustc at `~/.cargo/bin` (`. "$HOME/.cargo/env"`)
- Python venv for the parity scripts: `/mnt/raid0/Code/venvs/.astrovenv/bin/python3`
- `config.ini` now sits in this repo root (gitignored), upgraded to the v2.1.2 schema

**Both repos are pushed and in sync with origin.** Nothing uncommitted except
`SESSION.old.md` here and an untracked `.vscode/` in the Python repo (not mine).

## ✅ Completed Work

### Phase 5 — rayon + release matrix (`43f14de`, `a01c504`, `385552a`, `b0613bc`, `747de93`)
- `src/extractor.rs` — parallel file reads via `par_iter().map().collect()`, which
  preserves row order by construction (hazard 8). Measured on this machine's rotational
  disk with `RAYON_NUM_THREADS=1` vs default, same code path: **15.0 ms/file → 6.6 ms/file,
  2.3x**. The win is hiding seek latency, not CPU.
- `src/pathutil.rs` — split into `posix` and `windows` submodules, mirroring how CPython
  keeps `posixpath`/`ntpath` importable on any OS. Both are always compiled and always
  tested, which is what let `cargo test` here catch a Windows-only bug.
- `.github/workflows/release-matrix.yml` — five targets, **run for real three times** on
  GitHub runners, fixing what each run found: (1) Windows checkout failed on `MAX_PATH`
  (fixed with `core.longpaths`), (2) `posix::abspath`'s test depended on the host's real
  cwd shape (fixed by dependency injection — `abspath_with_cwd`), (3) all five green.

### Phase 6 — differential harness in CI (`f7fc55e`, `f05e13e`)
- `.github/workflows/differential-harness.yml` — all four `check_*.py` on every push to
  `main`, report-only. Verified by an actual run: all **sixteen** fixture/scenario checks
  passed on a clean runner, 1m30s.

### Two real bugs found by running against the user's actual data
1. **Zero exposure on master frames** — *not* a port bug. The dev-folder `config.ini` was
   the old v2.0.3 schema, missing `[override] EXPOSURE = EXPTIME`. PixInsight masters
   store exposure only under `EXPTIME`. Fixed by upgrading the config; live Python showed
   the identical wrong output with the same stale config.
2. **`MASTERxxx` labels on raw calibration frames** (`d89a112` here, `717e440` upstream) —
   a **genuine Python bug**, present before this port existed. Verified with live Python
   on the user's data first, then traced to a comment claiming "v1.4.7 standards" for
   behaviour v1.4.7 never had (checked the real v1.4.7 source, still on disk). Fixed
   upstream first, then ported identically. **Parity target bumped 2.1.1 → 2.1.2** across
   `check_steps.py`, `Cargo.toml`, and every forward-looking doc claim.

### Other work this session
- `feat(extractor)` (`c10a349`) — the `Scanning files: N of total...` progress line the
  current Python still prints. Gated behind a `progress: bool`; printing it unconditionally
  corrupted `--dump-steps`' stdout, caught by `check_readers.py` (3/3 → 0/3) before shipping.
- **Corpus grew to 4 CSV fixtures + 241 binary files** — `mosaic` and `lbn548` captured
  from real data and blessed; four unverifiable orphan references deleted; `mosaic` was
  re-blessed after the label fix.
- **Upstream issues #9, #10, #11 verified and closed** (`fc8899c`, `c309cd0`) — all three
  reproduced from the reports' own figures (the reporters' data was never available),
  cross-checked against Python, and pinned as named regression tests. **Tracker is now at
  zero open issues.**
- **Python v2.1.2 released** — tagged, published, marked latest:
  https://github.com/SteveGreaves/AstroBinUploader/releases/tag/v2.1.2
- **Python docs brought current** (`fae47fe`) — `README.md` had real staleness (module
  list gone since v2.0, a live `[secrets]`/API section for network calls removed in
  v2.1.0, a false `[sites]` auto-update claim, stale `ROTATOR`, and `[equipmentoverrides]`
  entirely undocumented). `future_work.md` reviewed item by item: 7 of 11 done, 1 partly,
  3 open.

## 🚧 Current Blockers & Technical Debt

- **Nothing blocking.** 135 tests, no warnings, all four parity harnesses green
  (`check_parity.py` 5/5, `check_steps.py` 4/4, `check_reports.py` 4/4,
  `check_readers.py` 3/3), CI green on every push.
- **Two open questions I raised and the user hasn't answered yet:**
  1. `v2.1.0` and `v2.1.1` were tagged but **never published as GitHub releases** — the
     Releases page skipped straight from v2.0.3 to the new v2.1.2. Backfill them from
     their changelog entries?
  2. An untracked `.vscode/` sits in the Python repo. Add to `.gitignore`?
- **No release has been cut for the Rust port itself.** No tag, no published binaries —
  only CI artifacts. This was the natural next step before the bug reports intervened.
- **Nobody has run the Windows/macOS binaries on real hardware** — CI-built and CI-tested
  only. First real Windows use is also the first real test of `pathutil::windows` outside
  its unit tests.
- Fixture gaps still on record in `parity/CORPUS.md`: **`DARKFLAT`/`MASTERDARKFLATS`**
  (200 frames exist at `Preselected/Calibration data/24th February 2022/FlatWizard/` but
  need a same-era lights set to pair with), a **multi-site session** (the site loop has
  never run twice), and a **blank filter in a flat table**.
- Deliberately narrow paths, each saying so in code: `datetime.rs` ISO-8601 only;
  `steps::promote` same-dtype and int/float only; `pandas_fmt.rs`'s scientific-notation
  and empty-frame branches; `fits.rs` has no `CONTINUE`/`HIERARCH` support (measured: no
  such card exists in the 227-file corpus).
- `src/pathutil.rs`'s `windows` module is correct per `ntpath` but has only ever run in
  tests — no real Windows path has gone through it.
- The differential harness is meant to **retire** after one release of overlap
  (`PORT_PLAN.md` decision 4), not live forever. Not yet — no release to overlap with.
- The user is **content** that their home address appears in the public upstream repo's
  golden references. Do not raise it again.

## 🚀 Next Steps

1. **Answer the two open questions above** (backfill v2.1.0/v2.1.1 releases; `.vscode/`
   gitignore) — both are small and were left hanging when context ran out.
2. **Cut a release for the Rust port.** This is the actual next milestone: pick a version
   (mirror `2.1.2`, or start at `1.0.0` since it's a distinct artifact — the user's call),
   tag it, and publish the five binaries `release-matrix.yml` already builds. That
   workflow triggers on `push: tags: ["v*"]`, so tagging should produce them; verify with
   `gh run watch` rather than assuming.
3. **Consider running the Windows binary on real hardware** before advertising it.
4. Only if a matching dataset turns up: close the `DARKFLAT` / multi-site / blank-filter
   fixture gaps. Each needs real data, not a synthetic guess.

The method that carried every phase, and both bug hunts, still applies:

1. **Measure the library, don't reason about it.** Every surprise this session — the
   `EXPTIME` config gap, the false "v1.4.7 standards" comment, the Windows `MAX_PATH`
   checkout failure, the `abspath` test that only broke on a non-POSIX host, the progress
   line corrupting `--dump-steps` — came from running something and reading the answer.
2. **Check which side is actually wrong.** Twice this session a reported "Rust bug" was
   reproduced identically by live Python. Run the oracle on the user's exact data *before*
   touching any code.
3. **Diff strictly**, byte for byte, no line stripping — `reports.py`'s `{:<15}` padding
   leaves trailing spaces a normalising diff would hide.
4. **Pin every verified finding as a named test**, so it survives past the session that
   found it.

## 📂 Files to Load

- `PORT_PLAN.md` — plan of record. All six phases done; the hazard list is still the best
  map of where the difficulty lives.
- `parity/CORPUS.md` — corpus provenance, what was re-blessed and why, and the "Not yet
  covered by any fixture" gaps.
- The four harnesses — run all of them first to confirm a green starting state:
  `parity/check_parity.py`, `check_steps.py`, `check_reports.py`, `check_readers.py`.
- `.github/workflows/release-matrix.yml` — what a Rust-port release will trigger.
- `SESSION.old.md` — the previous handoff, if deeper history on Phases 1–4 is needed.
- Upstream, if the Python side comes up: `../AstroBinUploader/CHANGELOG.md` and its
  `REMEDIATION_PLAN.md` reconciliation table (per-issue status and evidence).

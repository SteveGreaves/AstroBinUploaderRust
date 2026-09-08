# astrobin-upload (Rust)

A standalone Rust port of
[AstroBinUploader](https://github.com/SteveGreaves/AstroBinUploader) — a
metadata ETL pipeline that turns a directory of FITS/XISF astrophotography
frames into the acquisition CSV and session summary AstroBin's bulk importer
expects.

The goal is a single self-contained binary per platform — Windows, Linux and
macOS — with no Python, no libcfitsio and no shared-library requirements. The
FITS header reader is hand-written for exactly that reason; the whole binary
depends on `clap`, `anyhow`, `chrono` and `roxmltree`.

## Status: complete (6 of 6)

Functionally complete and released for five platforms: a directory of
FITS/XISF frames in, both artifacts out, byte-identical to Python v2.1.2, with
`rayon` parallelism on the disk scan, a CI-verified build for Linux (`musl`,
static), Windows (x86-64 and arm64) and macOS (x86-64 and arm64), and a
differential harness confirming parity against the live Python oracle on
every push.

| Phase | Scope | State |
|---|---|---|
| 1 | CLI, config parser, `--test` CSV ingest | **done** |
| 2 | The six pipeline steps | **done** |
| 3 | Exporter and report formatting | **done** |
| 4 | FITS and XISF readers | **done** |
| 5 | Parallelism, release matrix | **done** |
| 6 | Differential harness in CI | **done** |

```sh
$ astrobin-upload "/data/Sadr Region"
# scans recursively, then writes Sadr_Region_acquisition.csv and
# Sadr_Region_session_summary.txt into
# /data/Sadr Region/AstroBinUploadInfo/
```

See [`PORT_PLAN.md`](PORT_PLAN.md) for the full plan, the parity contract and
the ranked hazard list.

## The parity contract

> The Rust binary reproduces, byte for byte, the `*_acquisition.csv` and
> `*_session_summary.txt` produced by Python **v2.1.2** for every fixture in
> `parity/fixtures/`, with the sole exception of the `Generated <timestamp>`
> line.

Byte-for-byte parity with a pandas program is the whole difficulty of this
port, and most of it lives in two places: the `DataFrame.to_string()` table
appended to the session summary, and the many small pandas semantics the
pipeline leans on — rounding mode, group-key ordering, null propagation,
`agg('first')` vs `.iloc[0]`. `PORT_PLAN.md` ranks all fourteen.

## Emulated libraries are checked, not assumed

Two Python libraries are reimplemented here rather than substituted:

- **configobj** — its format is not standard INI. Sections nest by bracket
  count, section names may be quoted so they can contain commas, a
  comma-separated value becomes a list, and nothing is type-coerced (`0` is
  the string `"0"`). No Rust INI crate does this.
- **`pandas.read_csv` dtype inference** — which decides whether a gain of 100
  renders as `100` or `100.0` in the file AstroBin consumes. `None` is a null
  sentinel, `True`/`False` infer as bool, leading zeros are lost to int64, and
  an integer past i64 stays a string rather than becoming a float.
- **`pd.DataFrame(list_of_dicts)` inference**, which is a *different* set of
  rules and is what a disk scan goes through. Here `None` is not a sentinel —
  it stays the literal string — and a bool column with one missing key becomes
  `object` rather than `bool`.
- **astropy's FITS header presentation.** A tile-compressed HDU is reported as
  the image it decompresses to, not as the BINTABLE on disk, so `XTENSION`
  reads `IMAGE`, the `Z`-prefixed cards replace the table's geometry, and the
  compression machinery disappears. The reader reproduces that rather than
  reporting what is literally in the file.

`parity/check_parity.py` builds the same canonical dump from the real
libraries and from this binary, then diffs them:

```
$ cargo build && python3 parity/check_parity.py
[PASS] config: golden_config.ini  (59 lines)
[PASS] csv dtypes: sadr           (124 lines)
[PASS] csv dtypes: sh2101_calib   (119 lines)
```

It needs `configobj` and `pandas` installed, since those are what it compares
against.

Two more harnesses sit on top of it, and all three must stay green:

```
$ python3 parity/check_steps.py      # every pipeline step, cell by cell
[PASS] sadr: 465 lines identical  (00_raw, 01_NormalizeHeadersStep, ...)
[PASS] sh2101_calib: 450 lines identical  (00_raw, ...)
[PASS] mosaic: 444 lines identical  (00_raw, ...)
[PASS] lbn548: 450 lines identical  (00_raw, ...)

$ python3 parity/check_reports.py    # the two output artifacts
[PASS] sadr: both artifacts byte-identical; temperature statistics bit-identical over 1 site(s)
...
4/4 fixture(s) passed.

$ python3 parity/check_readers.py    # the FITS and XISF readers, off real files
[PASS] Sadr Region: 221 file(s) scanned; 478 lines identical (00_raw, ...); both artifacts match references/sadr_*
...
3/3 scenario(s) passed.
```

## Releases

```sh
gh workflow run release-matrix.yml
```

builds and (where the runner's own CPU can execute the result) tests all five
targets: `x86_64-unknown-linux-musl` (fully static), `x86_64-pc-windows-msvc`,
`aarch64-pc-windows-msvc`, `x86_64-apple-darwin`, `aarch64-apple-darwin`. No
target needs a C toolchain of its own beyond musl's final link step — the
whole point of hand-writing the FITS reader instead of binding cfitsio.

## Continuous parity checking

`.github/workflows/differential-harness.yml` runs all four `parity/check_*.py`
scripts on every push to `main` — the live Python oracle and this port, side
by side, on GitHub's own runners. Report-only: a divergence shows as a failed
step, not a failed build, since this repo has no PR workflow yet for a
blocking check to gate. Meant to retire once there has been a release to
overlap with (`PORT_PLAN.md` decision 4), not stay forever.

`check_steps.py` compares the live Python pipeline's frames against this
binary's, in a canonical form that survives the trip (floats as raw IEEE-754
bits, so no repr disagreement can hide a difference). `check_reports.py`
byte-compares the finished CSV and summary against the committed references,
and then compares the temperature statistics the summary *rounds away* — a
green summary says nothing about how a mean was summed, and one of them is
numpy's pairwise reduction rather than the Kahan sum the rest of the pipeline
uses. `check_readers.py` scans the committed binary fixtures with both
implementations; the `Sadr Region` scenario is 221 real N.I.N.A. frames and is
compared against the *same* reference the CSV fixture uses, so the reader is
checked against a target that existed before it did.

## Build

```sh
cargo build --release      # target/release/astrobin-upload
cargo test                 # 132 unit tests
```

## Corpus

`parity/` holds copies of the upstream golden fixtures and references so this
repository tests standalone. [`parity/CORPUS.md`](parity/CORPUS.md) records
which upstream commit they came from and when to re-copy them.

## Licence

GPL-3.0-or-later, matching the upstream project.

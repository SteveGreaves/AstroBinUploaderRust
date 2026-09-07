# astrobin-upload (Rust)

A standalone Rust port of
[AstroBinUploader](https://github.com/SteveGreaves/AstroBinUploader) — a
metadata ETL pipeline that turns a directory of FITS/XISF astrophotography
frames into the acquisition CSV and session summary AstroBin's bulk importer
expects.

The goal is a single self-contained binary per platform — Windows, Linux and
macOS — with no Python, no libcfitsio and no shared-library requirements.

## Status: Phase 1 of 6

This does not process images yet. See [`PORT_PLAN.md`](PORT_PLAN.md) for the
full plan, the parity contract and the ranked hazard list.

| Phase | Scope | State |
|---|---|---|
| 1 | CLI, config parser, `--test` CSV ingest | **done** |
| 2 | The six pipeline steps | not started |
| 3 | Exporter and report formatting | not started |
| 4 | FITS and XISF readers | not started |
| 5 | Parallelism, release matrix | not started |
| 6 | Differential harness in CI | not started |

## The parity contract

> The Rust binary reproduces, byte for byte, the `*_acquisition.csv` and
> `*_session_summary.txt` produced by Python **v2.1.1** for every fixture in
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

## Build

```sh
cargo build --release      # target/release/astrobin-upload
cargo test                 # 19 unit tests
```

## Corpus

`parity/` holds copies of the upstream golden fixtures and references so this
repository tests standalone. [`parity/CORPUS.md`](parity/CORPUS.md) records
which upstream commit they came from and when to re-copy them.

## Licence

GPL-3.0-or-later, matching the upstream project.

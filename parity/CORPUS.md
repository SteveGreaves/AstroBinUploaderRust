# Parity corpus — provenance

The files in this directory are **copies** taken from the Python project they
exist to test against:

- Source: <https://github.com/SteveGreaves/AstroBinUploader> `golden_tests/`
- Taken at: `d2f61bcb17ed2fc6ad76c3b400318f651a2f5408` (`v2.1.1-5-gd2f61bc`)
- Copied: 2026-09-07

| Here | Upstream |
|---|---|
| `fixtures/` | `golden_tests/fixtures/` |
| `references/` | `golden_tests/references/` |
| `golden_config.ini` | `golden_tests/golden_config.ini` |

They are duplicated rather than referenced so this repository builds and tests
on its own, with no second checkout and no submodule.

## When to re-copy

Whenever the Python side re-blesses its references — any change to Bucket A
behaviour, or to `golden_config.ini` — these copies go stale and the
differential harness starts comparing against the wrong baseline. Re-copy the
three paths above and update the commit recorded here in the same change.

## Do not regenerate `fixtures/sadr_raw.csv`

It predates remediation A2 and therefore carries no `SOURCE_PATH` column,
which makes it the only fixture exercising the *degraded* filename-only
deduplication branch (and its warning). `sh2101_calib_raw.csv` exercises the
normal `(directory, basename)` path. Regenerating sadr would silently drop
half that coverage. See `PORT_PLAN.md` hazard 13.

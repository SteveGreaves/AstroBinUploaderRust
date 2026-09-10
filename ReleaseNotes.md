# Release Notes - AstroBin Upload Utility (Rust edition)

## [v2.2.1] - 2026-09-09
### Parity with released Python v2.2.0/v2.2.1

The previous release (`v2.1.2`) matched Python up to v2.1.3 and had no
network layer at all. This release closes that gap and brings the port up to
Python's currently released v2.2.0/v2.2.1 behaviour.

**Restored — reverse geocoding and live sky quality lookups.** A `[secret]`
section with a valid API key and e-mail address now resolves an unrecognised
observing site the same way Python does: OpenStreetMap's Nominatim supplies
the postal address, lightpollutionmap.info supplies the artificial
brightness (converted to SQM and then Bortle), and the result is written
back into `[sites]` so the site is only ever looked up once. Requests go
over `ureq` with rustls and bundled root certificates, so this needs no
system TLS library or CA store on any platform — unlike Python's
`requests`/`geopy`. Every failure mode (no key, no network, a refused or
malformed response) degrades to `[defaults]` and lets the run finish, same
as Python.

**`[secret]` is now parsed.** Previously silently dropped; now carried into
the config model and read by the network layer above.

**The utility now generates its own `config.ini`.** Run with no arguments
and no existing config, it writes a default one — including the same
per-section explanatory comments Python's generator adds — and exits, the
same first-run behaviour as the Python original. `config.ini.example`
is rebuilt from this generator's own output, correcting several values that
had drifted from what the code actually defaults to (`USEOBSDATE`, `XPIXSZ`,
`HFR`, `SITE`, and a stale `FWHM` key that is never read).

**`--test` resolves a bare filename correctly.** Looked for first in the
run's own `AstroBinUploadInfo` directory, then at the path exactly as given —
so replaying your own `debug_step_00_RawHeaders.csv` takes the bare
filename, matching Python.

**Fixed — an aggregation dtype divergence found by live testing, not by any
fixture.** A calibration-master session with no per-frame `FOCTEMP` header
could report `temp_min`/`temp_max` as `20.0` in the `--debug` dump where
Python wrote `20`. Traced to `pd.to_numeric`'s text-shape-based int64/float64
inference on a Stage-3-default-injected string column, which the port had
been flattening to float64 unconditionally. Never affected the acquisition
CSV or session summary — only the `--debug` dump.

**Python source log-line literals resynced** to the released v2.2.0 line
numbers, so the log's `Line:` fields still name the right Python source line
for every record.

Nothing else changed. Every artifact the port already produced —
acquisition CSV, session summary, all `debug_step_*.csv` files — remains
byte-for-byte identical to Python's, verified on both the committed corpus
and, for the network layer specifically, live runs against real data and
the real Nominatim/lightpollutionmap.info services.

---

## [v2.1.2] - initial Rust port release

The first published release of the Rust port. A single self-contained
executable reproducing AstroBinUploader v2.1.3's output byte-for-byte: the
same acquisition CSV, the same session summary, the same `debug_step_*.csv`
files, and a log carrying the same records the Python original does (Python
source `funcName`/`Line:` fields included, since that is what a reader
comparing the two logs expects to see).

Covers the full pipeline — normalization, optical parameters, deduplication,
calibration matching, geocoding against a local `[sites]` database, and
aggregation — plus hand-written FITS and XISF readers (no `libcfitsio`
dependency), `rayon`-parallelised header extraction, and release binaries
for Linux (`musl`, statically linked), Windows and macOS, Intel/AMD and ARM.

Verified against the Python oracle by a differential test harness run on
every change: both programs over the same corpus, byte-compared.

**Known gap at this release, closed in v2.2.1 above:** no network code at
all. An unrecognised observing site fell back to `[defaults]` regardless of
`[secret]`, and `config.ini` had to be hand-written or copied from
`config.ini.example` rather than generated.

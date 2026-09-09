# AstroBin Upload Utility - Program Overview (Rust edition)

## Purpose
The AstroBin Upload Utility is an automated metadata extraction and aggregation tool designed to streamline the "Bulk Upload" process for AstroBin. It scans your imaging directories, identifies light and calibration frames, and produces the specific CSV and text reports required for accurate session documentation.

This document describes the Rust port, a single self-contained executable
whose output is byte-for-byte identical to the original
[AstroBinUploader](https://github.com/SteveGreaves/AstroBinUploader) Python
utility at v2.2.0. Everything below describes the Rust implementation
specifically; see `packaging/README.md`'s
[Differences from the Python utility](packaging/README.md#differences-from-the-python-utility)
for the handful of deliberate differences, such as no `~` path expansion.

## Architecture: The Pipeline Pattern
The application separates the work into three surrounding components and a pipeline of six independent, testable Steps.

**Around the pipeline:**

- **Loader** (`src/config.rs`, `src/appconfig.rs`, `src/config_write.rs`): Discovers FITS and XISF files and manages configuration profiles (supporting custom `.ini` files via `--config`).
- **Extractor** (`src/extractor.rs`, `src/fits.rs`, `src/xisf.rs`): High-speed parallel parsing (via `rayon`) of XML and binary headers.
- **Exporter** (`src/exporter.rs`, `src/reports.rs`): Generates the final AstroBin-ready CSV and the human-readable session summary.

**The pipeline itself** — the six steps registered in `src/steps/mod.rs`, in execution order. The stage numbers below are the ones used in the source comments, carried over from the Python original:

1.  **Stage 1 — Normalization (`steps/normalize.rs`)**: Sanitizes inconsistent metadata, applies user-defined `[override]` and `[equipmentoverrides]` mappings, and fills gaps from `[defaults]`.
2.  **Stage 2 — Optical Parameters (`steps/optical.rs`)**: Derives resolution and star metrics — Image Scale (IMSCALE) and FWHM from measured or estimated HFR.
3.  **Stage 3 — Deduplication (`steps/deduplicate.rs`)**: Identifies and removes redundant files (e.g. WBPP postfixes).
4.  **Stage 4 — Calibration Matching (`steps/calibration.rs`)**: Associates Darks, Flats and Bias frames using the Hybrid Handshake (EGAIN/GAIN).
5.  **Stage 5 — Geocoding (`steps/geocode.rs`)**: Resolves each set of coordinates to a named site. See Site Resolution below.
6.  **Stage 6 — Aggregation (`steps/aggregate.rs`)**: Vectorized statistical reduction of thousands of frames into session-level summaries.

## Key Logic Components

### The Hybrid Handshake
To ensure calibration frames belong to the correct lights, the utility uses a multi-factor "handshake":
- **Primary**: Electronic Gain signature (`E_0.25`).
- **Secondary**: Linear Integer Gain (`G_100`).
- **Required**: Binning and Filter (for Flats).

### Master Preference
If both raw subs and a Master integration exist for the same hardware group, the utility gives "Master Preference" to the integration. It discards the redundant raws and uses the integrated count from the master's history.

### Site Resolution
`GeocodeStep` resolves coordinates to a site in three tiers, stopping at the first that succeeds:

1.  **Smart Proximity Clustering**: GPS readings drift between sessions, so coordinates within `CLUSTER_RADIUS_M` (110 m, measured with a vectorized haversine) are treated as one physical site, and the cluster's centroid — the mean of all its readings — becomes that site's canonical position. Clustering is greedy single-linkage: once a point joins a cluster it stays there.
2.  **`[sites]` database lookup**: A fuzzy coordinate match against the sites already recorded in `config.ini`. A hit short-circuits, so a known site costs nothing and never touches the network.
3.  **External lookup** (`src/sites.rs`, a port of `engine/sites.py` restored in v2.2.0): Only when the coordinates are new. OpenStreetMap's Nominatim supplies the postal address and lightpollutionmap.info's World Atlas 2015 layer supplies the artificial brightness, converted to SQM and then to a Bortle class. The result is written back into `[sites]`, so each site is looked up exactly once.

Tier 3 uses the API key and e-mail address in the standard `[secret]` section. Without a valid key the sky quality comes from `[defaults] BORTLE`/`SQM`; if the address request fails the site details come from `[defaults] SITE`, `SITELAT` and `SITELONG`. Every failure mode — an unedited placeholder key, no network, a refused or malformed response — degrades the same way and lets the run finish, making no further network calls. Requests go over `ureq` with rustls and bundled root certificates (`src/sites.rs`'s `HttpTransport` trait), rather than Python's `requests`/`geopy` — so, unlike the Python original, there is no library to install and no system TLS store or OpenSSL dependency on any platform.

### Vectorization
Python's statistical operations are performed using Pandas vectorized logic rather than Python loops. This port's `Table` type (`src/table.rs`) reproduces the same reductions — Kahan-summed and pairwise-summed to match pandas' own summation order bit-for-bit (see `src/steps/mod.rs`'s `kahan_sum`/`pairwise_sum`) — over plain Rust data structures, with `rayon` used for the embarrassingly-parallel header extraction rather than for the aggregation itself.

## Debugging and Testing
The system is built for high transparency and robust error recovery:
-   **Raw Data Capture**: `debug_step_00_RawHeaders.csv` stores the metadata exactly as read from disk. This is the **only supported source** for standard re-testing via the `--test` flag.
-   **Emergency Diagnostics**: A fatal crash writes `emergency_raw_dump.csv`, preserving scanned metadata for immediate recovery using the `--test` flag — provided the crash happened after extraction. A crash before any headers are read has nothing to dump and produces only the log.
-   **Traceability**: Every file's raw header is logged horizontally (DEBUG level) upon extraction.
-   **Sequential Dumps**: Intermediate dataframes are exported after each pipeline step in `--debug` mode for stage-by-stage auditing.
-   **Full Exception Capture**: Global error handling ensures all crashes record a failing step and its error message in the log file — the equivalent of Python's full traceback, since a compiled program has no traceback to unwind.

## Usage
The utility is a single self-contained executable — no Python, no virtual environment, no libraries to install. Call it directly:

    ./astrobin-upload [directory_paths] [options]

On Windows:

    astrobin-upload.exe [directory_paths] [options]

| Argument | Purpose |
| --- | --- |
| `directory_paths` | One or more directories to scan recursively for `.fits`, `.fit`, `.fts` or `.xisf` files. **Omit them entirely on a first run**: with no paths and no `config.ini`, a default configuration is generated and the program exits so it can be edited. With no paths and a `config.ini` already present, it reports that a directory is needed and exits. |
| `--config`, `-c` | Use a named configuration file instead of `config.ini`. |
| `--debug` | Verbose logging, and preserve the intermediate dataframe from every step. |
| `--test CSV_FILE` | Diagnostic mode: inject metadata from a CSV of already-extracted headers instead of scanning disk. Looked for first in the run's `AstroBinUploadInfo` directory — so a bare filename replays that run's own `debug_step_00_RawHeaders.csv` or `emergency_raw_dump.csv` — then at the path as given, relative or absolute. |

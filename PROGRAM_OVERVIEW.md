# AstroBin Upload Utility - Program Overview

## Purpose
The AstroBin Upload Utility is an automated metadata extraction and aggregation tool designed to streamline the "Bulk Upload" process for AstroBin. It scans your imaging directories, identifies light and calibration frames, and produces the specific CSV and text reports required for accurate session documentation.

This document explains how the program works. `README.md` is the full manual —
installation, configuration, worked examples and troubleshooting.

## Architecture: The Pipeline Pattern
The application separates the work into three surrounding components and a pipeline of six independent stages.

**Around the pipeline:**

- **Loader**: Reads and validates `config.ini`, including custom profiles supplied with `--config`.
- **Extractor**: High-speed parallel parsing of FITS and XISF headers, using every available CPU core.
- **Exporter**: Generates the final AstroBin-ready CSV and the human-readable session summary.

**The pipeline itself** — six stages, in execution order:

1.  **Stage 1 — Normalization**: Sanitizes inconsistent metadata, applies user-defined `[override]` and `[equipmentoverrides]` mappings, and fills gaps from `[defaults]`.
2.  **Stage 2 — Optical Parameters**: Derives resolution and star metrics — Image Scale (IMSCALE) and FWHM from measured or estimated HFR.
3.  **Stage 3 — Deduplication**: Identifies and removes redundant files (e.g. WBPP postfixes).
4.  **Stage 4 — Calibration Matching**: Associates Darks, Flats and Bias frames using the Hybrid Handshake (EGAIN/GAIN).
5.  **Stage 5 — Geocoding**: Resolves each set of coordinates to a named site. See Site Resolution below.
6.  **Stage 6 — Aggregation**: Statistical reduction of thousands of frames into session-level summaries.

## Key Logic Components

### The Hybrid Handshake
To ensure calibration frames belong to the correct lights, the utility uses a multi-factor "handshake":
- **Primary**: Electronic Gain signature (`E_0.25`).
- **Secondary**: Linear Integer Gain (`G_100`).
- **Required**: Binning and Filter (for Flats).

### Master Preference
If both raw subs and a Master integration exist for the same hardware group, the utility gives "Master Preference" to the integration. It discards the redundant raws and uses the integrated count from the master's history.

### Site Resolution
Coordinates are resolved to a site in three tiers, stopping at the first that succeeds:

1.  **Smart Proximity Clustering**: GPS readings drift between sessions, so coordinates within 110 m of each other are treated as one physical site, and the cluster's centroid — the mean of all its readings — becomes that site's canonical position. Clustering is greedy single-linkage: once a point joins a cluster it stays there.
2.  **`[sites]` database lookup**: A fuzzy coordinate match against the sites already recorded in `config.ini`. A hit short-circuits, so a known site costs nothing and never touches the network.
3.  **External lookup**: Only when the coordinates are new. OpenStreetMap's Nominatim supplies the postal address and lightpollutionmap.info's World Atlas 2015 layer supplies the artificial brightness, converted to SQM and then to a Bortle class. The result is written back into `[sites]`, so each site is looked up exactly once.

Tier 3 uses the API key and e-mail address in the `[secret]` section. Without a valid key the sky quality comes from `[defaults] BORTLE`/`SQM`; if the address request fails the site details come from `[defaults] SITE`, `SITELAT` and `SITELONG`. Every failure mode — an unedited placeholder key, no network, a refused or malformed response — degrades the same way and lets the run finish, making no further network calls. TLS and the root certificates are built into the executable, so no system certificate store or TLS library is needed on any platform.

### Session Statistics
Frame counts, exposure totals, and mean temperature, gain and FWHM are reduced across every frame in a session at once, rather than one row at a time. Summation is order-stable, so the same data always produces the same figures to the last decimal place.

## Debugging and Testing
The system is built for high transparency and robust error recovery:
-   **Raw Data Capture**: `debug_step_00_RawHeaders.csv` stores the metadata exactly as read from disk. This is the **only supported source** for standard re-testing via the `--test` flag.
-   **Emergency Diagnostics**: A fatal crash writes `emergency_raw_dump.csv`, preserving scanned metadata for immediate recovery using the `--test` flag — provided the crash happened after extraction. A crash before any headers are read has nothing to dump and produces only the log.
-   **Traceability**: Every file's raw header is logged horizontally (DEBUG level) upon extraction.
-   **Sequential Dumps**: Intermediate tables are exported after each pipeline stage in `--debug` mode for stage-by-stage auditing.
-   **Full Exception Capture**: Global error handling ensures a fatal error records the failing stage and its message in the log file, and that whatever was scanned is preserved.

## Usage
The utility is a single self-contained executable — nothing to install. Call it directly:

    ./astrobin-upload [directory_paths] [options]

On Windows:

    astrobin-upload.exe [directory_paths] [options]

`config.ini` is read from the directory you run the program in, and results are
written into the **first** directory you name, in a folder called
`AstroBinUploadInfo`.

| Argument | Purpose |
| --- | --- |
| `directory_paths` | One or more directories to scan recursively for `.fits`, `.fit`, `.fts` or `.xisf` files. **Omit them entirely on a first run**: with no paths and no `config.ini`, a default configuration is generated and the program exits so it can be edited. With no paths and a `config.ini` already present, it reports that a directory is needed and exits. |
| `--config`, `-c` | Use a named configuration file instead of `config.ini`. |
| `--debug` | Verbose logging, and preserve the intermediate table from every stage. |
| `--test CSV_FILE` | Diagnostic mode: inject metadata from a CSV of already-extracted headers instead of scanning disk. Looked for first in the run's `AstroBinUploadInfo` directory — so a bare filename replays that run's own `debug_step_00_RawHeaders.csv` or `emergency_raw_dump.csv` — then at the path as given, relative or absolute. |

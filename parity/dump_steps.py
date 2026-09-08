#!/usr/bin/env python3
"""
Lossless per-step dump of the Python pipeline, for Phase 2 verification.

Why not just diff the `--debug` CSVs the Python side already writes: because
`to_csv` -> `read_csv` is lossy in exactly the ways this port has to get
right. Measured on debug_step_01 for the sadr fixture:

    rotname     in memory: object, the string 'None'
                via CSV:   float64, all NaN        ('None' is an NA sentinel)
    useobsdate  in memory: object, the string 'False'
                via CSV:   bool

A comparison built on those files would show false agreement (both sides
equally corrupted) or false failure, with no way to tell which. So this walks
the pipeline in-process and dumps each step's DataFrame in a canonical form
that survives the trip:

    STEP<TAB>step_name<TAB>column<TAB>dtype<TAB>v1|v2|v3...

Floats are emitted as their raw IEEE-754 bits in hex, which is exact and
unambiguous in both languages -- Python's repr and Rust's Display disagree on
values like 1e20, and that disagreement is not something this dump should be
testing.

The first argument is either a captured CSV (the `--test` injection point) or
a **directory**, in which case the frame comes from a real disk scan through
`extract_from_directories` -- which is what Phase 4's readers have to
reproduce. The two paths build their frames differently and it matters:
`extract_from_csv` goes through `read_csv` dtype inference and upper-cases
every column name, while a scan builds `pd.DataFrame(list_of_dicts)` from
whatever Python objects astropy and the XISF parser returned, and does not
touch the names.

Usage:
    python3 parity/dump_steps.py <fixture.csv|directory> <config.ini> [--step N]

Requires the Python project importable; point PYTHONPATH at it or run with
--python-repo.
"""

import argparse
import struct
import sys
from pathlib import Path

NULL = "\x00"      # distinguishable from any real value
SEP = "|"


def canon(value, dtype_kind: str) -> str:
    """One cell, rendered so Rust can produce the identical byte sequence."""
    import numpy as np
    import pandas as pd

    if value is None or (isinstance(value, float) and pd.isna(value)):
        return NULL
    if dtype_kind == "f":
        return "f" + struct.pack("<d", float(value)).hex()
    if dtype_kind == "i":
        return "i%d" % int(value)
    if dtype_kind == "b":
        return "b1" if bool(value) else "b0"
    # object / string column -- NaN is still possible inside one
    if isinstance(value, float) and np.isnan(value):
        return NULL
    s = str(value)
    return "s" + s.replace("\\", "\\\\").replace(SEP, "\\p").replace("\n", "\\n")


def dump_frame(step_name: str, df, out) -> None:
    import pandas as pd

    # The column list, on its own line, before the columns themselves.
    # Column order is load-bearing and, on the disk-scan path, is decided by
    # first appearance across every file in the scan -- so one unexpected card
    # in the first file shifts every later column and the per-column diff
    # becomes 60 shifted lines with no obvious cause. This line makes that
    # failure one short diff instead.
    print(f"COLS\t{step_name}\t{SEP.join(str(c) for c in df.columns)}", file=out)

    for col in df.columns:
        series = df[col]
        kind = series.dtype.kind  # i, f, b, O, M ...
        if kind == "M":  # datetime64 -> ISO, matching how it is later consumed
            rendered = [
                NULL if pd.isna(v) else "s" + str(v) for v in series
            ]
            kind_tag = "datetime64"
        else:
            rendered = [canon(v, kind) for v in series]
            kind_tag = str(series.dtype)
        print(
            f"STEP\t{step_name}\t{col}\t{kind_tag}\t{SEP.join(rendered)}",
            file=out,
        )


def extract(extractor, source: str):
    """The captured-CSV path or the disk-scan path, whichever `source` names.

    `extract_from_directories` writes a progress counter to stdout, which is
    where the dump goes, so it is redirected to stderr for the duration --
    otherwise "Scanning files: 1 of 5..." lands in the middle of the frame.
    """
    import contextlib
    import os

    if os.path.isdir(source):
        with contextlib.redirect_stdout(sys.stderr):
            return extractor.extract_from_directories([source])
    return extractor.extract_from_csv(source)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("fixture", help="a captured CSV, or a directory to scan")
    ap.add_argument("config")
    ap.add_argument("--python-repo", default=None,
                    help="path to the AstroBinUploader checkout (default: sibling)")
    ap.add_argument("--step", type=int, default=None,
                    help="dump only this step number (1-6)")
    args = ap.parse_args()

    repo = Path(args.python_repo) if args.python_repo else \
        Path(__file__).resolve().parent.parent.parent / "AstroBinUploader"
    if not (repo / "AstroBinUpload.py").exists():
        raise SystemExit(f"AstroBinUploader checkout not found at {repo}")
    sys.path.insert(0, str(repo))

    import logging
    logging.getLogger("AstroBinV2").addHandler(logging.NullHandler())
    logging.getLogger("AstroBinV2").setLevel(logging.CRITICAL)

    from engine.loader import ConfigLoader
    from engine.extractor import HeaderExtractor
    from engine.steps.base import NormalizeHeadersStep
    from engine.steps.optical import OpticalParameterStep
    from engine.steps.deduplicate import DeduplicateStep
    from engine.steps.calibration import CalibrationMatcherStep
    from engine.steps.geocode import GeocodeStep
    from engine.steps.aggregate import AggregationStep
    from models import SessionState

    logger = logging.getLogger("AstroBinV2")
    config = ConfigLoader(logger).load(args.config)
    raw_df = extract(HeaderExtractor(logger, config), args.fixture)
    state = SessionState(config=config, raw_df=raw_df)

    steps = [
        ("NormalizeHeadersStep", NormalizeHeadersStep()),
        ("OpticalParameterStep", OpticalParameterStep()),
        ("DeduplicateStep", DeduplicateStep()),
        ("CalibrationMatcherStep", CalibrationMatcherStep()),
        ("GeocodeStep", GeocodeStep()),
        ("AggregationStep", AggregationStep()),
    ]

    dump_frame("00_raw", raw_df, sys.stdout)
    for i, (name, step) in enumerate(steps, 1):
        state = step.execute(state)
        if args.step is not None and args.step != i:
            continue
        frame = state.aggregated_df if name == "AggregationStep" else state.processed_df
        dump_frame(f"{i:02d}_{name}", frame, sys.stdout)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

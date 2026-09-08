#!/usr/bin/env python3
"""Differential check of the Phase 3 exporter and reports.

Two independent comparisons, because they fail for different reasons:

1.  **The artifacts.** Replays each fixture through the Rust binary in the
    same scenario-directory layout `golden_tests/run_golden.py` uses upstream,
    then byte-compares `<basename>_acquisition.csv` and
    `<basename>_session_summary.txt` against the committed references. Only
    the `Generated <timestamp>` line is normalised, exactly as upstream does;
    nothing else is stripped, because `reports.py`'s `{:<15}` padding leaves
    trailing spaces a "helpful" diff would hide.

2.  **The statistics the summary rounds away.** "Mean temperature" prints one
    decimal place, so a green artifact diff says nothing about *how* that mean
    was summed -- and it is the codebase's only bare `Series.mean()`, i.e. the
    only numpy **pairwise** summation rather than the Kahan-compensated one
    every groupby uses (PORT_PLAN.md hazard 4). This step runs pandas on the
    same aggregated frame and compares the raw IEEE-754 bits.

    Without it the port's pairwise transcription would be unverified: the
    corpus cannot see the difference.

    python3 parity/check_reports.py [--release] [--keep DIR]

Environment: as `check_steps.py` -- ASTROBIN_PY_REPO and ASTROBIN_PYTHON.
"""

import argparse
import pathlib
import re
import shutil
import struct
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent
sys.path.insert(0, str(HERE))

from check_steps import CONFIG, FIXTURES, PARITY_TARGET, PY_REPO, PYTHON, check_oracle, run

REFERENCES = HERE / "references"
_GENERATED_LINE = re.compile(r"^Generated .*$", re.MULTILINE)


def normalise(text: str) -> str:
    """Strip the one line that legitimately differs run to run."""
    return _GENERATED_LINE.sub("Generated <TIMESTAMP>", text)


def first_diff(label: str, want: str, got: str) -> str:
    want_lines, got_lines = want.splitlines(), got.splitlines()
    for i, (w, g) in enumerate(zip(want_lines, got_lines)):
        if w != g:
            return (f"{label} differs at line {i + 1}:\n"
                    f"    reference: {w!r}\n"
                    f"    actual:    {g!r}")
    if len(want_lines) != len(got_lines):
        return f"{label} line count differs: reference={len(want_lines)} actual={len(got_lines)}"
    return f"{label} differs (byte-level only, e.g. the trailing newline)"


def python_temp_stats(fixture: pathlib.Path) -> str:
    """The same `SITE` lines the binary's --dump-report-stats emits.

    Runs in a subprocess so pandas and the Python checkout are imported by the
    interpreter that has them, not by whatever runs this script.
    """
    script = r'''
import struct, sys, logging
sys.path.insert(0, sys.argv[3])
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
from constants import ImageType, InternalColumns

logger = logging.getLogger("AstroBinV2")
config = ConfigLoader(logger).load(sys.argv[2])
raw_df = HeaderExtractor(logger, config).extract_from_csv(sys.argv[1])
state = SessionState(config=config, raw_df=raw_df)
for step in (NormalizeHeadersStep(), OpticalParameterStep(), DeduplicateStep(),
             CalibrationMatcherStep(), GeocodeStep(), AggregationStep()):
    state = step.execute(state)
df = state.aggregated_df

def bits(v):
    return "f" + struct.pack("<d", float(v)).hex()

# Exactly reports.py's own traversal: groupby(site), lights subset, skip
# calibration-only sites.
for site, site_group in df.groupby(InternalColumns.SITE_NAME, observed=True):
    lights = site_group[site_group[InternalColumns.IMAGE_TYPE] == ImageType.LIGHT.value]
    if lights.empty:
        continue
    print("SITE\t%s\t%d\t%s\t%s\t%s" % (
        site, len(lights),
        bits(lights[InternalColumns.TEMP_MIN].min()),
        bits(lights[InternalColumns.TEMP_MAX].max()),
        bits(lights[InternalColumns.TEMPERATURE].mean()),
    ))
'''
    proc = run([PYTHON, "-c", script, str(fixture), str(CONFIG), str(PY_REPO)])
    return proc.stdout


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--release", action="store_true", help="use the release binary")
    ap.add_argument("--keep", metavar="DIR", help="write the run tree here instead of a temp dir")
    args = ap.parse_args()

    profile = "release" if args.release else "debug"
    exe = REPO / "target" / profile / "astrobin-upload"
    if not exe.exists():
        print(f"binary not built: {exe} (cargo build{' --release' if args.release else ''})")
        return 2

    problems = check_oracle(PY_REPO)
    if problems:
        for p in problems:
            print(f"[STALE] {p}")
        return 2
    describe = run(["git", "-C", str(PY_REPO), "describe", "--tags", "--always"]).stdout.strip()
    print(f"oracle: {PY_REPO} v{PARITY_TARGET} ({describe})\n")

    root = pathlib.Path(args.keep) if args.keep else pathlib.Path(tempfile.mkdtemp())
    root.mkdir(parents=True, exist_ok=True)

    failures = 0
    for name, fixture in FIXTURES:
        # The exporter names its output after the basename of the first
        # directory argument, so the scratch directory has to carry the
        # fixture's original basename for the filenames -- and the summary's
        # embedded CSV name -- to match.
        sidecar = HERE / "fixtures" / f"{name}.basename"
        basename = sidecar.read_text(encoding="utf-8").strip() if sidecar.exists() else name

        work = root / name / basename
        if work.exists():
            shutil.rmtree(work)
        work.mkdir(parents=True)
        local_csv = work / "raw.csv"
        shutil.copy(fixture, local_csv)

        proc = subprocess.run(
            [str(exe), str(work), "--test", str(local_csv), "--config", str(CONFIG)],
            capture_output=True, text=True, timeout=600,
        )
        if proc.returncode != 0:
            print(f"[FAIL] {name}\n  binary exited {proc.returncode}\n{proc.stderr[-2000:]}")
            failures += 1
            continue

        out_dir = work / "AstroBinUploadInfo"
        problems = []
        for label, produced, reference in (
            ("acquisition CSV",
             out_dir / f"{basename}_acquisition.csv",
             REFERENCES / f"{name}_acquisition.csv"),
            ("summary",
             out_dir / f"{basename}_session_summary.txt",
             REFERENCES / f"{name}_summary.txt"),
        ):
            if not produced.exists():
                problems.append(f"{label}: the binary produced no {produced.name}")
                continue
            got = produced.read_text(encoding="utf-8")
            want = reference.read_text(encoding="utf-8")
            if label == "summary":
                got, want = normalise(got), normalise(want)
            if got != want:
                problems.append(first_diff(label, want, got))

        # The precision the summary throws away.
        rust_stats = subprocess.run(
            [str(exe), str(work), "--test", str(local_csv), "--config", str(CONFIG),
             "--dump-report-stats"],
            capture_output=True, text=True, timeout=600, check=True,
        ).stdout
        py_stats = python_temp_stats(fixture)
        if rust_stats != py_stats:
            problems.append(first_diff("temperature statistics", py_stats, rust_stats))

        if problems:
            print(f"[FAIL] {name}")
            for p in problems:
                print(f"  {p}")
            failures += 1
        else:
            n_sites = len(py_stats.strip().splitlines())
            print(f"[PASS] {name}: both artifacts byte-identical; "
                  f"temperature statistics bit-identical over {n_sites} site(s)")

    if args.keep:
        print(f"\nrun tree kept at {root}")
    else:
        shutil.rmtree(root, ignore_errors=True)

    if failures:
        print(f"\n{failures}/{len(FIXTURES)} fixture(s) failed.")
        return 1
    print(f"\n{len(FIXTURES)}/{len(FIXTURES)} fixture(s) passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

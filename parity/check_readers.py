#!/usr/bin/env python3
"""Differential check of the FITS and XISF readers -- Phase 4.

Two comparisons per scenario in `fixtures/binary/`, for the same reason
`check_reports.py` has two:

1.  **The raw frame, cell by cell.** Both sides scan the *same* directory and
    dump every pipeline step in the canonical form (floats as raw IEEE-754
    bits). `00_raw` is the readers' own output; the five steps after it come
    along free and catch anything the readers got subtly wrong that the
    artifacts would round away.

    The scan happens **in place**, in the committed corpus, and deliberately:
    `SOURCE_PATH` is an absolute path, so copying the scenario somewhere first
    would make every row differ unless both sides were pointed at the identical
    copy. A dump writes nothing -- but only because `--dump-steps` returns
    before the exporter runs, which is an ordering property of `main`, not of
    this harness. So the run ends by asking
    `make_binary_fixtures.py --check` whether anything appeared in the corpus,
    and fails if it did.

2.  **The artifacts**, from a copy in a scratch directory -- a scan writes
    `AstroBinUploadInfo/` beside the files, which must not land in the corpus.
    `Sadr Region` is compared against `references/sadr_*`: the same reference
    the CSV fixture uses, so the reader is checked against a target that
    existed before it did.

    python3 parity/check_readers.py [--release] [--keep DIR]
"""

import argparse
import pathlib
import shutil
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent
sys.path.insert(0, str(HERE))

from check_steps import CONFIG, PARITY_TARGET, PY_REPO, PYTHON, check_oracle, run
from check_reports import first_diff, normalise

BINARY = HERE / "fixtures" / "binary"
REFERENCES = HERE / "references"

# (scenario directory, reference stem). "Sadr Region" points at the reference
# the CSV fixture already used -- the scan and the replay have to agree.
SCENARIOS = [
    ("Sadr Region", "sadr"),
    ("xisf_mixed", "binary_xisf_mixed"),
    ("synthetic", "binary_synthetic"),
]


def dumps_agree(exe: pathlib.Path, scenario: pathlib.Path, outdir: pathlib.Path, name: str):
    """Runs both dumps over `scenario` in place and compares them line by line."""
    py_path = outdir / f"py_{name}.txt"
    rs_path = outdir / f"rs_{name}.txt"
    py = run([PYTHON, str(HERE / "dump_steps.py"), str(scenario), str(CONFIG)])
    py_path.write_text(py.stdout, encoding="utf-8")
    rs = run([str(exe), str(scenario), "--config", str(CONFIG), "--dump-steps"])
    rs_path.write_text(rs.stdout, encoding="utf-8")

    if py.stdout == rs.stdout:
        steps = [l.split("\t")[1] for l in py.stdout.splitlines() if l.startswith("COLS")]
        return None, f"{len(py.stdout.splitlines())} lines identical  ({', '.join(steps)})"

    py_lines, rs_lines = py.stdout.splitlines(), rs.stdout.splitlines()
    for i, (a, b) in enumerate(zip(py_lines, rs_lines)):
        if a != b:
            # A COLS line is short enough to print whole; a value line is one
            # column of up to 1693 cells, so narrow.py localises it instead.
            if a.startswith("COLS"):
                return (f"column order differs at line {i + 1}:\n"
                        f"    reference: {a}\n"
                        f"    actual:    {b}"), None
            field = a.split("\t")
            hint = run([PYTHON, str(HERE / "narrow.py"), str(py_path), str(rs_path)],
                       check=False).stdout if (HERE / "narrow.py").exists() else ""
            return (f"line {i + 1} differs, step {field[1]}, column {field[2]}\n"
                    + (hint or "")), None
    return f"line count differs: python={len(py_lines)} rust={len(rs_lines)}", None


def artifacts_agree(exe: pathlib.Path, scenario: pathlib.Path, stem: str, work: pathlib.Path):
    """Scans a *copy* and byte-compares both output files against the reference."""
    basename = scenario.name.replace(" ", "_")
    target = work / scenario.name
    shutil.copytree(scenario, target)
    proc = subprocess.run(
        [str(exe), str(target), "--config", str(CONFIG)],
        capture_output=True, text=True, timeout=900,
    )
    if proc.returncode != 0:
        return [f"binary exited {proc.returncode}\n{proc.stderr[-2000:]}"]

    out_dir = target / "AstroBinUploadInfo"
    problems = []
    for label, produced, reference in (
        ("acquisition CSV", out_dir / f"{basename}_acquisition.csv",
         REFERENCES / f"{stem}_acquisition.csv"),
        ("summary", out_dir / f"{basename}_session_summary.txt",
         REFERENCES / f"{stem}_summary.txt"),
    ):
        if not produced.exists():
            problems.append(f"{label}: the binary produced no {produced.name}")
            continue
        got, want = produced.read_text(encoding="utf-8"), reference.read_text(encoding="utf-8")
        if label == "summary":
            got, want = normalise(got), normalise(want)
        if got != want:
            problems.append(first_diff(label, want, got))
    return problems


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--release", action="store_true", help="use the release binary")
    ap.add_argument("--keep", metavar="DIR", help="write the dumps and scans here")
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
    for name, stem in SCENARIOS:
        scenario = BINARY / name
        if not scenario.is_dir():
            print(f"[FAIL] {name}: no such scenario at {scenario}")
            failures += 1
            continue

        slug = name.replace(" ", "_")
        bad, detail = dumps_agree(exe, scenario, root, slug)
        if bad:
            print(f"[FAIL] {name}\n  {bad}")
            failures += 1
            continue

        work = root / f"scan_{slug}"
        work.mkdir(parents=True, exist_ok=True)
        artifact_problems = artifacts_agree(exe, scenario, stem, work)
        if artifact_problems:
            print(f"[FAIL] {name}")
            for p in artifact_problems:
                print(f"  {p}")
            failures += 1
            continue

        n_files = sum(1 for p in scenario.rglob("*") if p.is_file())
        print(f"[PASS] {name}: {n_files} file(s) scanned; {detail}; "
              f"both artifacts match references/{stem}_*")

    if args.keep:
        print(f"\nkept at {root}")
    else:
        shutil.rmtree(root, ignore_errors=True)

    # The scans above ran inside the committed corpus. Prove they left it
    # exactly as they found it rather than assuming so.
    integrity = subprocess.run(
        [sys.executable, str(HERE / "make_binary_fixtures.py"), "--check"],
        capture_output=True, text=True,
    )
    if integrity.returncode != 0:
        print("\n[FAIL] the corpus changed during the run:")
        print(integrity.stdout.strip())
        failures += 1

    if failures:
        print(f"\n{failures}/{len(SCENARIOS)} scenario(s) failed.")
        return 1
    print(f"\n{len(SCENARIOS)}/{len(SCENARIOS)} scenario(s) passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

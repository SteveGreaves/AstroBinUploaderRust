#!/usr/bin/env python3
"""Differential check of the ported pipeline steps, fixture by fixture.

Runs `dump_steps.py` (the live Python pipeline) and `astrobin-upload
--dump-steps` (this port) over each fixture and compares them byte for byte
over the prefix the Rust side emits -- so unported steps are simply absent
rather than failing.

    python3 parity/check_steps.py [--release] [--keep tmpdir]

Environment: ASTROBIN_PY_REPO points at the Python checkout the oracle imports
(default: the sibling AstroBinUploader), ASTROBIN_PYTHON at an interpreter with
pandas and configobj installed. Both are verified before anything runs -- see
check_oracle().

Exit status is 0 when every fixture agrees. A mismatch prints the offending
columns and rows via narrow.py rather than dumping the raw diff, because one
column is a single line of up to 1693 values.
"""

import argparse
import filecmp
import os
import pathlib
import re
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent
# Fixtures copied from the Python project's own golden_tests/ -- these are
# verified still identical to it below, because a silent drift there would
# move the baseline without moving the reference.
UPSTREAM_FIXTURES = [
    ("sadr", HERE / "fixtures" / "sadr_raw.csv"),
    ("sh2101_calib", HERE / "fixtures" / "sh2101_calib_raw.csv"),
]
# Fixtures captured for this port and blessed against live Python v2.1.2.
# They have no upstream counterpart, so the staleness check must not look for
# one; parity/CORPUS.md records where each came from.
LOCAL_FIXTURES = [
    ("mosaic", HERE / "fixtures" / "mosaic_raw.csv"),
    ("lbn548", HERE / "fixtures" / "lbn548_raw.csv"),
    ("ic405", HERE / "fixtures" / "ic405_raw.csv"),
]
FIXTURES = UPSTREAM_FIXTURES + LOCAL_FIXTURES
CONFIG = HERE / "golden_config.ini"

# The oracle runs the *live* Python pipeline, so the checkout it imports is as
# much a part of the baseline as the fixtures are -- and unlike the fixtures,
# nothing here is a committed copy. Both are verified below.
PARITY_TARGET = "2.2.1"
PYTHON = os.environ.get(
    "ASTROBIN_PYTHON", "/mnt/raid0/Code/venvs/.astrovenv/bin/python3"
)
PY_REPO = pathlib.Path(
    os.environ.get("ASTROBIN_PY_REPO", REPO.parent / "AstroBinUploader")
)


def run(cmd, **kw):
    return subprocess.run(cmd, check=True, text=True, capture_output=True, **kw)


def check_oracle(repo: pathlib.Path) -> list:
    """Fail loudly if the oracle is no longer the version we claim parity with.

    Two ways this goes wrong silently, both of which have bitten this project
    before: the sibling checkout drifts off the parity target (every run still
    prints PASS, against a different baseline), or the committed corpus copies
    go stale against the upstream `golden_tests/` they were taken from.
    """
    problems = []
    version_py = repo / "_version.py"
    if not version_py.exists():
        return [f"no Python checkout at {repo} (set ASTROBIN_PY_REPO)"]

    text = version_py.read_text(encoding="utf-8")
    found = re.search(r"__version__\s*=\s*['\"]([^'\"]+)", text)
    version = found.group(1) if found else "?"
    if version != PARITY_TARGET:
        problems.append(
            f"{repo} is at v{version}, but the parity contract names "
            f"v{PARITY_TARGET} (PORT_PLAN.md, parity/CORPUS.md)"
        )

    upstream = repo / "golden_tests"
    if not upstream.exists():
        # golden_tests/ was untracked upstream (test data stays local, not on
        # GitHub) -- absent here is the policy working as intended, not
        # drift. Skip the fixture-staleness comparison rather than failing
        # the whole oracle check over it; the version guard above still runs.
        print(
            f"[SKIP] {upstream} not present -- upstream fixture-staleness "
            f"check skipped (golden_tests/ is local-only by policy)"
        )
        return problems
    pairs = [(CONFIG, upstream / "golden_config.ini")]
    pairs += [(f, upstream / "fixtures" / f.name) for _, f in UPSTREAM_FIXTURES]
    for ours, theirs in pairs:
        if not theirs.exists():
            problems.append(f"upstream copy missing: {theirs}")
        elif not filecmp.cmp(ours, theirs, shallow=False):
            problems.append(
                f"{ours.name} differs from {theirs} -- the corpus copies are "
                f"stale; re-copy and update parity/CORPUS.md"
            )
    return problems


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--release", action="store_true", help="use the release binary")
    ap.add_argument("--keep", metavar="DIR", help="write the dumps here instead of a temp dir")
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
    describe = run(
        ["git", "-C", str(PY_REPO), "describe", "--tags", "--always"]
    ).stdout.strip()
    print(f"oracle: {PY_REPO} v{PARITY_TARGET} ({describe})\n")

    outdir = pathlib.Path(args.keep) if args.keep else pathlib.Path(tempfile.mkdtemp())
    outdir.mkdir(parents=True, exist_ok=True)

    failures = 0
    for name, fixture in FIXTURES:
        py_path = outdir / f"py_{name}.txt"
        rs_path = outdir / f"rs_{name}.txt"

        py = run([PYTHON, str(HERE / "dump_steps.py"), str(fixture), str(CONFIG),
                  "--python-repo", str(PY_REPO)])
        py_path.write_text(py.stdout, encoding="utf-8")

        rs = run([str(exe), ".", "--test", str(fixture), "--config", str(CONFIG), "--dump-steps"])
        rs_path.write_text(rs.stdout, encoding="utf-8")

        py_lines = py.stdout.splitlines(keepends=True)
        rs_lines = rs.stdout.splitlines(keepends=True)
        if not rs_lines:
            print(f"[FAIL] {name}: the port emitted nothing")
            failures += 1
            continue

        # Compare only what the port claims to produce; column order is part
        # of the comparison, which is why this is a line-for-line prefix diff
        # and not a set comparison.
        prefix = py_lines[: len(rs_lines)]
        steps = sorted({line.split("\t")[1] for line in rs_lines})
        leftover = len(py_lines) - len(rs_lines)
        tail = "" if leftover == 0 else f"; {leftover} line(s) still unported"
        if prefix == rs_lines:
            print(f"[PASS] {name}: {len(rs_lines)} lines identical{tail}  ({', '.join(steps)})")
        else:
            failures += 1
            print(f"[FAIL] {name}: {', '.join(steps)}")
            sys.stdout.flush()
            subprocess.run(
                [sys.executable, str(HERE / "narrow.py"), str(py_path), str(rs_path)],
                check=False,
            )

    if args.keep:
        print(f"dumps kept in {outdir}")
    print("\nall step parity checks passed." if not failures else f"\n{failures} fixture(s) differ.")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())

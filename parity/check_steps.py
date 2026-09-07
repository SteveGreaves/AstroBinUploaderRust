#!/usr/bin/env python3
"""Differential check of the ported pipeline steps, fixture by fixture.

Runs `dump_steps.py` (the live Python pipeline) and `astrobin-upload
--dump-steps` (this port) over each fixture and compares them byte for byte
over the prefix the Rust side emits -- so unported steps are simply absent
rather than failing.

    python3 parity/check_steps.py [--release] [--keep tmpdir]

Exit status is 0 when every fixture agrees. A mismatch prints the offending
columns and rows via narrow.py rather than dumping the raw diff, because one
column is a single line of up to 1693 values.
"""

import argparse
import pathlib
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent
FIXTURES = [
    ("sadr", HERE / "fixtures" / "sadr_raw.csv"),
    ("sh2101_calib", HERE / "fixtures" / "sh2101_calib_raw.csv"),
]
CONFIG = HERE / "golden_config.ini"
PYTHON = "/mnt/raid0/Code/venvs/.astrovenv/bin/python3"


def run(cmd, **kw):
    return subprocess.run(cmd, check=True, text=True, capture_output=True, **kw)


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

    outdir = pathlib.Path(args.keep) if args.keep else pathlib.Path(tempfile.mkdtemp())
    outdir.mkdir(parents=True, exist_ok=True)

    failures = 0
    for name, fixture in FIXTURES:
        py_path = outdir / f"py_{name}.txt"
        rs_path = outdir / f"rs_{name}.txt"

        py = run([PYTHON, str(HERE / "dump_steps.py"), str(fixture), str(CONFIG)])
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

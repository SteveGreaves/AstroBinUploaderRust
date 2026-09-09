#!/usr/bin/env python3
"""Smoke-test a *released* binary on the machine it will actually run on.

The five `check_*.py` harnesses all import pandas and shell out to a live
Python checkout of AstroBinUploader, because they compare the port against the
Python utility. That is the wrong shape for the job this script does. Here the
question is not "does the port agree with Python" -- that is settled on the
development machine and in CI -- but "does *this build*, on *this* operating
system, still produce the bytes the references say it should".

So this script is deliberately **stdlib only**. No pandas, no configobj, no
second checkout, no cargo. Python 3.8+ and this directory are the whole
dependency list, which is what makes it runnable on a Windows or macOS box that
has nothing else installed.

Why it exists: every non-Linux binary this project ships has been built and
tested exclusively by CI. `src/pathutil.rs` carries a whole `windows` module --
`splitdrive`, `basename`, `dirname`, `isabs` -- that `#[cfg(windows)]` selects
only on a real Windows target, and no Windows machine has ever run it against
real data. The scan scenarios below are what would catch it: `SOURCE_PATH` never
reaches either output artifact, but `dirname`/`basename` decide the deduplication
key and the output filename, so a lexical path bug shows up as a byte difference
in files this script compares.

Usage:

    python3 parity/check_release.py <path-to-binary>
    py -3 parity\\check_release.py C:\\...\\astrobin-upload.exe

With no argument it looks for a locally built binary under `target/`.

Exit status is 0 when every scenario matches, 1 on any mismatch, 2 if the setup
itself is wrong (no binary, missing corpus).
"""

import argparse
import pathlib
import platform
import re
import shutil
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent
CONFIG = HERE / "golden_config.ini"
REFERENCES = HERE / "references"

# The one line that legitimately differs run to run, exactly as
# check_reports.py normalises it.
_GENERATED_LINE = re.compile(r"^Generated .*$", re.MULTILINE)

# Disk scans: (directory under fixtures/binary/, reference stem). These are the
# scenarios that exercise the platform's path handling, because the binary
# walks a real directory tree to find them.
SCANS = [
    ("Sadr Region", "sadr"),
    ("xisf_mixed", "binary_xisf_mixed"),
    ("synthetic", "binary_synthetic"),
]


def replays():
    """Every fixture that has a committed reference pair, discovered not listed.

    check_parity.py and check_log.py both glob `fixtures/*_raw.csv` rather than
    naming fixtures, so a new one is picked up by adding the file. This follows
    that, and additionally skips any fixture whose references are absent -- so
    the script is useful on a branch that has only some of them.

    No directory walk is involved in a replay, which is the point of having
    both halves: if a scan fails while its matching replay passes, the fault is
    in traversal or path handling rather than in the pipeline steps.
    """
    found = []
    for fixture in sorted((HERE / "fixtures").glob("*_raw.csv")):
        stem = fixture.name[: -len("_raw.csv")]
        if (REFERENCES / f"{stem}_acquisition.csv").exists():
            found.append((stem, stem))
    return found


def find_binary(explicit):
    if explicit:
        p = pathlib.Path(explicit).expanduser().resolve()
        return p if p.exists() else None
    name = "astrobin-upload.exe" if platform.system() == "Windows" else "astrobin-upload"
    for profile in ("release", "debug"):
        p = REPO / "target" / profile / name
        if p.exists():
            return p
    return None


def read(path):
    """Bytes, not text: the comparison is byte-for-byte by design."""
    return path.read_bytes()


def compare(label, produced, reference, normalise_generated):
    """Returns a problem string, or None when the two agree."""
    if not produced.exists():
        return f"{label}: the binary produced no {produced.name}"
    got, want = read(produced), read(reference)
    if got == want:
        return None

    if normalise_generated:
        g = _GENERATED_LINE.sub("Generated <TIMESTAMP>", got.decode("utf-8", "replace"))
        w = _GENERATED_LINE.sub("Generated <TIMESTAMP>", want.decode("utf-8", "replace"))
        if g == w:
            return None
        got_lines, want_lines = g.splitlines(), w.splitlines()
    else:
        got_lines = got.decode("utf-8", "replace").splitlines()
        want_lines = want.decode("utf-8", "replace").splitlines()

    # A CRLF reference against an LF binary is a checkout problem, not a port
    # problem, and it would otherwise present as every line differing at once.
    if b"\r\n" in want and b"\r\n" not in got:
        return (
            f"{label}: the reference file has CRLF line endings but the binary "
            f"writes LF.\n    This is git's autocrlf converting the corpus on "
            f"checkout, not a fault in the binary.\n    Fix with: git config "
            f"core.autocrlf false && git rm --cached -r . && git reset --hard"
        )

    for i, (w, g) in enumerate(zip(want_lines, got_lines)):
        if w != g:
            return (f"{label} differs at line {i + 1}:\n"
                    f"    reference: {w!r}\n"
                    f"    actual:    {g!r}")
    if len(want_lines) != len(got_lines):
        return (f"{label} line count differs: "
                f"reference={len(want_lines)} actual={len(got_lines)}")
    return f"{label} differs at the byte level only (e.g. a trailing newline)"


def check_artifacts(work, basename, stem):
    out = work / "AstroBinUploadInfo"
    problems = []
    for label, produced, reference, norm in (
        ("acquisition CSV", out / f"{basename}_acquisition.csv",
         REFERENCES / f"{stem}_acquisition.csv", False),
        ("summary", out / f"{basename}_session_summary.txt",
         REFERENCES / f"{stem}_summary.txt", True),
    ):
        problem = compare(label, produced, reference, norm)
        if problem:
            problems.append(problem)
    return problems


def run_binary(exe, args):
    try:
        return subprocess.run([str(exe)] + args, capture_output=True,
                              text=True, timeout=900)
    except subprocess.TimeoutExpired:
        return None


def scan_scenario(exe, root, scenario_dir, stem):
    """Copies the corpus tree out of the repo and scans the copy."""
    source = HERE / "fixtures" / "binary" / scenario_dir
    if not source.is_dir():
        return [f"missing corpus directory: {source}"], None
    target = root / scenario_dir
    shutil.copytree(source, target)

    proc = run_binary(exe, [str(target), "--config", str(CONFIG)])
    if proc is None:
        return ["the binary did not finish within 900s"], None
    if proc.returncode != 0:
        return [f"binary exited {proc.returncode}\n{proc.stderr[-2000:]}"], None

    basename = scenario_dir.replace(" ", "_")
    n = sum(1 for _ in source.rglob("*") if _.is_file())
    return check_artifacts(target, basename, stem), f"{n} file(s) scanned"


def replay_scenario(exe, root, fixture_stem, stem):
    """Injects a fixture CSV with --test; no directory walk involved."""
    fixture = HERE / "fixtures" / f"{fixture_stem}_raw.csv"
    if not fixture.exists():
        return [f"missing fixture: {fixture}"], None
    sidecar = HERE / "fixtures" / f"{fixture_stem}.basename"
    basename = sidecar.read_text(encoding="utf-8").strip() if sidecar.exists() else fixture_stem

    # The exporter names its output after the basename of the first directory
    # argument, so the scratch directory has to carry the fixture's original
    # basename for the filenames -- and the summary's embedded CSV name -- to
    # match. Same rule as check_reports.py.
    work = root / fixture_stem / basename
    work.mkdir(parents=True)
    local_csv = work / "raw.csv"
    shutil.copy(fixture, local_csv)

    proc = run_binary(exe, [str(work), "--test", str(local_csv),
                            "--config", str(CONFIG)])
    if proc is None:
        return ["the binary did not finish within 900s"], None
    if proc.returncode != 0:
        return [f"binary exited {proc.returncode}\n{proc.stderr[-2000:]}"], None

    with open(fixture, "rb") as fh:
        rows = sum(1 for _ in fh) - 1
    return check_artifacts(work, basename, stem), f"{rows} row(s) replayed"


def main():
    ap = argparse.ArgumentParser(
        description="Byte-compare a released binary's output against the committed references."
    )
    ap.add_argument("binary", nargs="?",
                    help="path to the astrobin-upload executable "
                         "(default: the newest build under target/)")
    ap.add_argument("--keep", metavar="DIR",
                    help="work here instead of a temporary directory")
    args = ap.parse_args()

    exe = find_binary(args.binary)
    if exe is None:
        print("No binary found. Pass the path to the one you downloaded, e.g.\n"
              "    python3 parity/check_release.py ./astrobin-upload")
        return 2
    if not CONFIG.exists() or not REFERENCES.is_dir():
        print(f"The parity corpus is not next to this script (looked in {HERE}).\n"
              "Run it from a checkout of the repository, not from the release archive.")
        return 2

    version = run_binary(exe, ["--version"])
    print(f"binary   : {exe}")
    print(f"version  : {(version.stdout or version.stderr).strip() if version else '?'}")
    print(f"platform : {platform.system()} {platform.release()} ({platform.machine()})")
    print(f"python   : {platform.python_version()}\n")

    root = pathlib.Path(args.keep) if args.keep else pathlib.Path(tempfile.mkdtemp())
    root.mkdir(parents=True, exist_ok=True)

    failures = 0
    total = 0
    for label, scenarios, fn in (
        ("scan", SCANS, scan_scenario),
        ("replay", replays(), replay_scenario),
    ):
        for first, stem in scenarios:
            total += 1
            problems, note = fn(exe, root, first, stem)
            if problems:
                failures += 1
                print(f"[FAIL] {label} {first}")
                for p in problems:
                    print(f"  {p}")
            else:
                print(f"[PASS] {label} {first}"
                      + (f"  ({note}; both artifacts byte-identical)" if note else ""))

    if args.keep:
        print(f"\nwork kept in {root}")
    else:
        shutil.rmtree(root, ignore_errors=True)

    if failures:
        print(f"\n{failures}/{total} scenario(s) failed on this platform.")
        return 1
    print(f"\n{total}/{total} scenario(s) passed. This build reproduces the "
          f"references byte for byte on {platform.system()}.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

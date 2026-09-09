#!/usr/bin/env python3
"""
Differential check for `AstroBinUploader.log`.

The Rust port writes the same log the Python utility does, in the same format
and with the same records in the same order. It cannot be byte-identical:
every line opens with a wall-clock timestamp, so two *Python* runs do not
match each other either. What is checkable, and what this pins, is everything
after the timestamp -- including the `%(funcName)s` and `%(lineno)d` fields,
which the port carries as literals naming the Python source each record stands
in for. Those literals are a snapshot of the Python side; this check is what
catches them drifting.

Two differences are expected and normalised away:

  * the run's own directory, which differs between the two output trees;
  * `Calling function and arguments provided:`, whose argv[0] is the Python
    script on one side and the binary on the other. Unfixable by construction,
    so the line is compared only up to argv[0].

Per-file extractor records are compared as a *set*, because both sides scan in
parallel: Python's counter advances under `as_completed()` and the port's under
rayon, so neither side's ordering is reproducible even against itself.

Usage:
    cargo build --release && python3 parity/check_log.py
"""

import http.server
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import threading

import os

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
PY_REPO = pathlib.Path(
    os.environ.get("ASTROBIN_PY_REPO", REPO_ROOT.parent / "AstroBinUploader")
)
PYTHON = os.environ.get(
    "ASTROBIN_PYTHON", "/mnt/raid0/Code/venvs/.astrovenv/bin/python3"
)
CONFIG = REPO_ROOT / "parity" / "golden_config.ini"
LOOKUP_CONFIG = REPO_ROOT / "parity" / "lookup_config.ini"
STUB_API_KEY = "ABCDEF1234567890"


def _rust_bin() -> pathlib.Path:
    """Release if it is there, debug otherwise -- CI builds only debug."""
    release = REPO_ROOT / "target" / "release" / "astrobin-upload"
    debug = REPO_ROOT / "target" / "debug" / "astrobin-upload"
    if release.exists() and (not debug.exists() or release.stat().st_mtime >= debug.stat().st_mtime):
        return release
    return debug


RUST_BIN = _rust_bin()

TS = re.compile(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} - ")
# The one record whose content cannot match: argv[0] names the entry point.
ARGV = " - INFO - Calling function and arguments provided: ["
PARALLEL = "extract_single_file - "


def run(cmd, cwd):
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)


def read_log(root: pathlib.Path, target: str) -> list[str]:
    log = root / target / "AstroBinUploadInfo" / "AstroBinUploader.log"
    if not log.exists():
        sys.exit(f"no log written at {log}")
    out = []
    for line in log.read_text(encoding="utf-8").splitlines():
        if not TS.match(line):
            sys.exit(f"log line does not open with a timestamp: {line!r}")
        line = TS.sub("", line, count=1)
        line = line.replace(str(root), "<DIR>")
        if ARGV in line:
            line = line.split(ARGV)[0] + ARGV + "<ARGV0>"
        out.append(line)
    return out


def compare(name: str, py: list[str], rs: list[str]) -> bool:
    # Parallel scan records: order is not reproducible on either side.
    py_par = sorted(l for l in py if l.startswith(PARALLEL))
    rs_par = sorted(l for l in rs if l.startswith(PARALLEL))
    py_seq = [l for l in py if not l.startswith(PARALLEL)]
    rs_seq = [l for l in rs if not l.startswith(PARALLEL)]

    ok = True
    if py_par != rs_par:
        only_py = set(py_par) - set(rs_par)
        only_rs = set(rs_par) - set(py_par)
        print(f"[FAIL] {name}: per-file records differ "
              f"({len(only_py)} only in Python, {len(only_rs)} only in Rust)")
        for l in list(only_py)[:3]:
            print(f"    python: {l[:160]}")
        for l in list(only_rs)[:3]:
            print(f"    rust  : {l[:160]}")
        ok = False

    if py_seq != rs_seq:
        print(f"[FAIL] {name}: ordered records differ")
        import difflib
        for l in list(difflib.unified_diff(py_seq, rs_seq, "python", "rust", n=1))[:20]:
            print(f"    {l.rstrip()[:200]}")
        ok = False

    if ok:
        print(f"[PASS] {name}: {len(py_seq)} ordered + {len(py_par)} per-file "
              f"records identical")
    return ok


def scenario_csv(tmp: pathlib.Path, fixture: pathlib.Path) -> tuple[list[str], list[str]]:
    logs = []
    for side, cmd in (
        ("py", [PYTHON, str(PY_REPO / "AstroBinUpload.py")]),
        ("rs", [str(RUST_BIN)]),
    ):
        root = tmp / f"csv_{side}"
        (root / "T").mkdir(parents=True)
        shutil.copy(fixture, root / "T" / "raw.csv")
        shutil.copy(CONFIG, root / "config.ini")
        run(cmd + ["T", "--test", "T/raw.csv", "--debug"], cwd=root)
        logs.append(read_log(root, "T"))
    return logs[0], logs[1]


def scenario_scan(tmp: pathlib.Path, corpus: pathlib.Path) -> tuple[list[str], list[str]]:
    logs = []
    for side, cmd in (
        ("py", [PYTHON, str(PY_REPO / "AstroBinUpload.py")]),
        ("rs", [str(RUST_BIN)]),
    ):
        root = tmp / f"scan_{side}"
        root.mkdir(parents=True)
        shutil.copytree(corpus, root / corpus.name)
        shutil.copy(CONFIG, root / "config.ini")
        run(cmd + [corpus.name, "--debug"], cwd=root)
        logs.append(read_log(root, corpus.name))
    return logs[0], logs[1]


class _StubSkyQualityHandler(http.server.BaseHTTPRequestHandler):
    """Answers every GET with a fixed World Atlas brightness value.

    Stands in for lightpollutionmap.info: both `get_bortle_sqm` (Python) and
    `sites::get_bortle_sqm` (Rust) send a GET with `key=` in the query
    string, and neither checks anything about the response beyond parsing
    the body as a float, so a fixed 200 with "0.1" drives the same
    conversion path a real success would.
    """

    def do_GET(self):  # noqa: N802 -- http.server's naming
        body = b"0.1"
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):  # silence -- this is a test fixture
        pass


def _start_stub_server() -> tuple[http.server.HTTPServer, str]:
    """A background HTTP server on an ephemeral loopback port.

    Started once and shared by both sides' subprocesses within one
    scenario run: it never distinguishes callers, and neither log records
    the port number on a successful lookup (only a *failed* request's
    message would carry the URL, which the redaction fix -- upstream
    v2.2.1 / this port -- exists to keep out of the log regardless).
    """
    server = http.server.HTTPServer(("127.0.0.1", 0), _StubSkyQualityHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    endpoint = f"http://127.0.0.1:{server.server_port}/"
    return server, endpoint


def scenario_lookup(tmp: pathlib.Path, corpus: pathlib.Path) -> tuple[list[str], list[str]]:
    """Drives `sites.py` / `sites.rs`'s log records through a local stub.

    Reaches: `resolve` (347/348/349/380), `get_bortle_sqm`
    (178/179/180/204/215), and `sqm_to_bortle` (76) -- six records, verified
    by running this scenario, not assumed from reading the source.

    Does **not** reach, and an earlier draft of this docstring wrongly
    claimed it did:

    - `save` (430/434) and the `[sites]` write-back it performs. No
      `EMAIL_ADDRESS` means `reverse_geocode` is never called, so the
      resolved site name falls back to `[defaults] SITE` -- and `save`'s own
      first guard (correctly) refuses to write the default-named site back
      under its own name. Confirmed by inspecting `config.ini` after the run
      on both sides: `[sites]` stays empty on both, and the log has no
      "Saved new site" line. Reaching `save` needs a site name that came
      from a real resolution, which needs reverse geocoding to succeed.
    - `reverse_geocode`'s own records (258/270/272/281) or `resolve`'s
      "neither could be resolved" (365) / "sky quality unavailable" (374)
      branches. Nominatim's domain is hardcoded on both sides, not
      config-driven, so it cannot be redirected at a local stub without
      changing code that would then differ from what ships.

    See PORT_PLAN.md's Phase 7 notes for what stays covered only by the
    live run against the real APIs.
    """
    server, endpoint = _start_stub_server()
    try:
        logs = []
        for side, cmd in (
            ("py", [PYTHON, str(PY_REPO / "AstroBinUpload.py")]),
            ("rs", [str(RUST_BIN)]),
        ):
            root = tmp / f"lookup_{side}"
            root.mkdir(parents=True)
            shutil.copytree(corpus, root / corpus.name)
            cfg_text = LOOKUP_CONFIG.read_text(encoding="utf-8")
            cfg_text = cfg_text.replace("ENDPOINT_PLACEHOLDER", endpoint)
            (root / "config.ini").write_text(cfg_text, encoding="utf-8")
            run(cmd + [corpus.name, "--debug"], cwd=root)
            logs.append(read_log(root, corpus.name))
        return logs[0], logs[1]
    finally:
        server.shutdown()


def main() -> int:
    if not RUST_BIN.exists():
        sys.exit(f"{RUST_BIN} not built -- run `cargo build` first")
    if not PY_REPO.exists():
        sys.exit(f"the Python oracle is not checked out at {PY_REPO}")

    failures = 0
    with tempfile.TemporaryDirectory() as td:
        tmp = pathlib.Path(td)
        for fixture in sorted((REPO_ROOT / "parity" / "fixtures").glob("*_raw.csv")):
            sub = tmp / fixture.stem
            sub.mkdir()
            py, rs = scenario_csv(sub, fixture)
            failures += not compare(f"--test {fixture.stem}", py, rs)

        corpus = REPO_ROOT / "parity" / "fixtures" / "binary" / "Sadr Region"
        if corpus.is_dir():
            sub = tmp / "scan"
            sub.mkdir()
            py, rs = scenario_scan(sub, corpus)
            failures += not compare(f"scan {corpus.name}", py, rs)

            sub = tmp / "lookup"
            sub.mkdir()
            py, rs = scenario_lookup(sub, corpus)
            failures += not compare(f"lookup {corpus.name}", py, rs)

    print()
    if failures:
        print(f"{failures} log check(s) failed.")
        return 1
    print("all log parity checks passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

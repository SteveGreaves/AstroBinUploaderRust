#!/usr/bin/env python3
"""Builds parity/fixtures/binary/ -- the corpus Phase 4's readers are tested on.

Neither this repository nor the Python project has ever had binary fixtures
(REMEDIATION_PLAN.md P0 item 3 was never done), so Phase 4 has to build its
own oracle corpus before it can build a reader.

Every file here is a **header-only truncation of a real capture**. The
pipeline reads headers and never touches pixel data, so cutting the file off
after the header loses nothing the port cares about and turns a 122 MB frame
into a few kilobytes. Verified: astropy returns an identical header dict from
the truncated copy (it warns about the missing data unit and reads on), and
the XISF reader only ever reads the XML block, so truncating just past it is
exact rather than approximate.

Real captures rather than synthesised ones, because the whole reason
remediation A7 exists is that a hand-built corpus only tests the cases its
author thought of. The one thing that *is* synthesised is the tile-compressed
`.fits.fz` case -- there is no such file anywhere on this machine, and it is
the single most important FITS branch to get right.

    python3 parity/make_binary_fixtures.py [--check]

`--check` verifies the existing corpus against its manifest without writing
anything. Sources live outside the repository, so this script only runs on the
maintainer's machine; the corpus it produces is committed and is what CI and
every other checkout actually use.
"""

import argparse
import hashlib
import json
import pathlib
import shutil
import struct
import sys

HERE = pathlib.Path(__file__).resolve().parent
BINARY = HERE / "fixtures" / "binary"
MANIFEST = BINARY / "MANIFEST.json"

ASTRO = pathlib.Path("/mnt/raid0/AstroImaging")
PIXINSIGHT = pathlib.Path("/home/steve/Desktop/Pixinsight/SH2 101")

# The full Sadr Region tree. 221 FITS files whose scan reproduces the
# committed sadr reference byte for byte, so Phase 4's FITS reader gets an
# end-to-end test against a reference that already exists -- and one that
# exercises traversal order over a real four-level tree, which a hand-built
# corpus cannot.
SADR_SRC = ASTRO / "Preselected" / "Sadr Region"
# The scenario directory keeps the original name, space and all: the exporter
# derives its output basename from it (`Sadr_Region`), so a scan of this tree
# lands on the committed reference's filenames without any special casing --
# and the space exercises the `.replace(" ", "_")` nothing else does.
SADR_SCENARIO = "Sadr Region"

# Curated XISF. Enough of each kind to exercise the branch, no more.
XISF_LIGHTS = ASTRO / "Preselected" / "SH2 101"
XISF_CAL = ASTRO / "Preselected" / "Calibration data" / "9th August 2026"
XISF_MASTERS = PIXINSIGHT / "master"
XISF_WBPP = PIXINSIGHT / "registered"

FITS_EXT = (".fits", ".fit", ".fts")


def fits_header_length(path: pathlib.Path) -> int:
    """Bytes up to and including the block holding the last HDU's END card.

    Walks HDU by HDU rather than stopping at the first END: a tile-compressed
    file keeps its real metadata in a later HDU, and truncating at HDU 0's END
    would throw away the very thing the reader has to find. Each HDU is a
    header of whole 2880-byte blocks followed by a data unit whose size comes
    from BITPIX/NAXIS, so the next header starts at a computable offset.
    """
    with path.open("rb") as f:
        offset = 0
        while True:
            cards, end_at = _read_header_blocks(f, offset)
            if cards is None:
                return offset
            offset = end_at
            data = _data_unit_size(cards)
            if data is None:
                return offset
            f.seek(offset + data)
            if not f.read(2880):
                return offset  # last HDU; the data unit is all that is left
            offset += data


def _read_header_blocks(f, offset: int):
    """Returns (cards, offset just past the END block), or (None, _) at EOF."""
    f.seek(offset)
    cards = {}
    pos = offset
    while True:
        block = f.read(2880)
        if len(block) < 2880:
            return (None, pos) if not cards else (cards, pos)
        pos += 2880
        for i in range(36):
            card = block[i * 80:(i + 1) * 80].decode("ascii", errors="replace")
            key = card[:8].strip()
            if key == "END":
                return cards, pos
            if key and "=" in card[8:10]:
                cards.setdefault(key, card[10:].split("/")[0].strip())


def _data_unit_size(cards):
    try:
        bitpix = int(cards["BITPIX"])
        naxis = int(cards["NAXIS"])
    except (KeyError, ValueError):
        return None
    n = 1
    for i in range(1, naxis + 1):
        try:
            n *= int(cards[f"NAXIS{i}"])
        except (KeyError, ValueError):
            return None
    if naxis == 0:
        n = 0
    size = n * abs(bitpix) // 8
    for key, mult in (("PCOUNT", 1), ("GCOUNT", 0)):
        pass
    size += int(cards.get("PCOUNT", 0) or 0)
    gcount = int(cards.get("GCOUNT", 1) or 1)
    size *= gcount
    return ((size + 2879) // 2880) * 2880 if size else 0


def xisf_header_length(path: pathlib.Path) -> int:
    """The 16-byte preamble plus the XML block -- all `_read_xisf` reads."""
    with path.open("rb") as f:
        sig = f.read(8)
        if sig != b"XISF0100":
            raise ValueError(f"{path}: not an XISF 1.0 file (signature {sig!r})")
        (length,) = struct.unpack("<I", f.read(4))
    return 16 + length


def truncate(src: pathlib.Path, dst: pathlib.Path) -> int:
    if src.suffix.lower() == ".xisf":
        n = xisf_header_length(src)
    elif src.suffix.lower() in FITS_EXT:
        n = fits_header_length(src)
    else:
        raise ValueError(f"{src}: not a FITS or XISF file")
    dst.parent.mkdir(parents=True, exist_ok=True)
    with src.open("rb") as fi, dst.open("wb") as fo:
        fo.write(fi.read(n))
    return n


def collect(root: pathlib.Path, limit=None, sort_key=None):
    files = sorted(
        p for p in root.rglob("*")
        if p.is_file() and p.suffix.lower() in (".xisf",) + FITS_EXT
        and "AstroBinUploadInfo" not in p.parts
    )
    if sort_key:
        files.sort(key=sort_key)
    return files[:limit] if limit else files


def build() -> dict:
    if BINARY.exists():
        shutil.rmtree(BINARY)
    manifest = {"scenarios": {}}

    # --- 1. The full Sadr FITS tree, structure preserved ------------------
    entries = []
    for src in collect(SADR_SRC):
        dst = BINARY / SADR_SCENARIO / src.relative_to(SADR_SRC)
        entries.append({"path": str(dst.relative_to(BINARY)), "bytes": truncate(src, dst)})
    manifest["scenarios"][SADR_SCENARIO] = {
        "source": str(SADR_SRC),
        "why": "221 N.I.N.A. FITS; a scan of this tree reproduces the committed "
               "sadr reference, and its four-level structure exercises traversal order",
        "files": entries,
    }

    # --- 2. Curated XISF --------------------------------------------------
    entries = []
    picks = []
    # Two lights from each of two filters: same target, different exposure/gain.
    for sub in ("ha", "blue"):
        picks += [(p, f"xisf_mixed/lights/{sub}/{p.name}")
                  for p in collect(XISF_LIGHTS / sub, limit=2)]
    # One raw frame of each calibration type.
    for sub in ("Dark", "Bias", "Flat/Ha", "Flat/Blue"):
        picks += [(p, f"xisf_mixed/calibration/{sub}/{p.name}")
                  for p in collect(XISF_CAL / sub, limit=1)]
    # Masters: the numberOfImages HISTORY lives here, and these headers are
    # ~190 KiB because PixInsight records every input frame.
    for name in ("masterBias_BIN-1_9576x6388_GAIN-100.xisf",
                 "masterDark_BIN-1_9576x6388_EXPOSURE-600.00s_GAIN-100.xisf",
                 "masterFlat_BIN-1_9576x6388_FILTER-Ha_mono_GAIN-100.xisf"):
        picks.append((XISF_MASTERS / name, f"xisf_mixed/master/{name}"))
    # WBPP postfix chains (_c_lps_r) -- the dedup regex's real input.
    wbpp = collect(XISF_WBPP, limit=3)
    picks += [(p, f"xisf_mixed/registered/{p.parent.name}/{p.name}") for p in wbpp]

    for src, rel in picks:
        if not src.exists():
            raise SystemExit(f"source missing, refusing to build a partial corpus: {src}")
        entries.append({"path": rel, "bytes": truncate(src, BINARY / rel)})
    manifest["scenarios"]["xisf_mixed"] = {
        "source": f"{XISF_LIGHTS}, {XISF_CAL}, {XISF_MASTERS}, {XISF_WBPP}",
        "why": "lights, one raw frame per calibration type, three PixInsight masters "
               "(ImageIntegration.numberOfImages in HISTORY) and WBPP _c_lps_r names",
        "files": entries,
    }

    manifest["scenarios"]["synthetic"] = {
        "source": "generated by make_synthetic_fits.py",
        "why": "branches no real file on this machine reaches",
        "files": [],
    }

    for scenario in manifest["scenarios"].values():
        for e in scenario["files"]:
            e["sha256"] = hashlib.sha256((BINARY / e["path"]).read_bytes()).hexdigest()
    MANIFEST.write_text(json.dumps(manifest, indent=1) + "\n", encoding="utf-8")
    return manifest


def check() -> int:
    if not MANIFEST.exists():
        print(f"no manifest at {MANIFEST}")
        return 2
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    bad = 0
    for name, scenario in manifest["scenarios"].items():
        for e in scenario["files"]:
            p = BINARY / e["path"]
            if not p.exists():
                print(f"[MISSING] {e['path']}")
                bad += 1
            elif hashlib.sha256(p.read_bytes()).hexdigest() != e["sha256"]:
                print(f"[CHANGED] {e['path']}")
                bad += 1
        print(f"[{'FAIL' if bad else 'OK'}] {name}: {len(scenario['files'])} file(s)")
    return 1 if bad else 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true",
                    help="verify the committed corpus against its manifest")
    args = ap.parse_args()
    if args.check:
        return check()
    manifest = build()
    total = 0
    for name, scenario in manifest["scenarios"].items():
        n = sum(e["bytes"] for e in scenario["files"])
        total += n
        print(f"{name:12} {len(scenario['files']):4} file(s)  {n/1024:8.1f} KiB")
    print(f"{'total':12} {'':4}          {total/1024:8.1f} KiB")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

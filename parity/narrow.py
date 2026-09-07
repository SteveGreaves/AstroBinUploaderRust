#!/usr/bin/env python3
"""Localise a parity mismatch to individual rows.

`dump_steps.py` puts a whole column on one line, so `diff` says *which column*
differs and nothing more. This walks two canonical dumps, pairs them on
(step, column), and prints the first few differing row indices with both
values -- floats also decoded from their IEEE-754 hex so the number is
readable.

    python3 parity/narrow.py <expected.txt> <actual.txt> [--max N]
"""
import argparse
import struct
import sys


def load(path):
    out = {}
    order = []
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            parts = line.rstrip("\n").split("\t")
            if len(parts) != 5 or parts[0] != "STEP":
                continue
            _, step, col, dtype, values = parts
            key = (step, col)
            out[key] = (dtype, values.split("|"))
            order.append(key)
    return out, order


def show(v):
    if v.startswith("f") and len(v) == 17:
        try:
            return f"{v} ({struct.unpack('<d', bytes.fromhex(v[1:]))[0]!r})"
        except ValueError:
            pass
    return "NULL" if v == "\x00" else v


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("expected")
    ap.add_argument("actual")
    ap.add_argument("--max", type=int, default=5, help="rows shown per column")
    args = ap.parse_args()

    exp, exp_order = load(args.expected)
    act, _ = load(args.actual)

    bad = 0
    for key in exp_order:
        if key not in act:
            continue  # step not ported yet; the prefix is what is compared
        step, col = key
        (ed, ev), (ad, av) = exp[key], act[key]
        if ed != ad:
            print(f"{step}\t{col}\tDTYPE expected={ed} actual={ad}")
            bad += 1
        if len(ev) != len(av):
            print(f"{step}\t{col}\tLENGTH expected={len(ev)} actual={len(av)}")
            bad += 1
            continue
        diffs = [i for i, (a, b) in enumerate(zip(ev, av)) if a != b]
        if diffs:
            bad += 1
            print(f"{step}\t{col}\t{len(diffs)}/{len(ev)} rows differ")
            for i in diffs[: args.max]:
                print(f"    row {i}: expected {show(ev[i])}")
                print(f"           actual   {show(av[i])}")

    missing = [k for k in exp_order if k not in act]
    extra = [k for k in act if k not in exp]
    if extra:
        print(f"columns present only in {args.actual}: {extra}")
        bad += 1
    if missing:
        print(f"({len(missing)} column(s) not emitted by {args.actual}; "
              "expected while steps are unported)")
    print("MATCH" if not bad else f"{bad} mismatching column(s)")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())

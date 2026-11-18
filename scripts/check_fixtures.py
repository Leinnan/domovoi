#!/usr/bin/env python3
"""Fails if committed fixtures still contain account or credential material.

Usage: scripts/check_fixtures.py fixtures/ [--forbid STRING ...]
--forbid adds literal strings known to be sensitive on this machine (emails,
tokens) without writing them into the repo.
"""
import argparse
import pathlib
import re
import sys

PATTERNS = {
    "email": re.compile(r"[A-Za-z0-9._%+-]+@(?!example\.com)[A-Za-z0-9.-]+\.[A-Za-z]{2,}"),
    "bearer": re.compile(r"(Bearer|DPoP)\s+(?!<redacted>)[A-Za-z0-9._~+/=-]{8,}"),
    "query-secret": re.compile(r"(token|ticket|dpop)=(?!<redacted>)[A-Za-z0-9._~-]{6,}"),
    "home-path": re.compile(r"/Users/(?!HOME)[a-z][^/\"]*/"),
}

ap = argparse.ArgumentParser()
ap.add_argument("root")
ap.add_argument("--forbid", action="append", default=[])
a = ap.parse_args()

bad = 0
for p in sorted(pathlib.Path(a.root).rglob("*")):
    if not p.is_file():
        continue
    text = p.read_text(errors="replace")
    for name, rx in PATTERNS.items():
        for m in rx.finditer(text):
            print(f"{p}: {name}: {m.group(0)[:60]}")
            bad += 1
    for s in a.forbid:
        if s and s in text:
            print(f"{p}: forbidden literal found")
            bad += 1
print("fixtures clean" if bad == 0 else f"{bad} finding(s)")
sys.exit(1 if bad else 0)

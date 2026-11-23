#!/usr/bin/env python3
"""Where the bytes of a Mach-O binary go: each architecture's segments and
largest sections, and, when the binary still has its symbol table, its code
by owning crate (`--crates`).

Generic code from std and serde is charged to the first non-std crate in its
type arguments (serde_json decoding `domovoi_model` types counts as
`domovoi_model`). Release builds are stripped, so for `--crates` build one
with `CARGO_PROFILE_RELEASE_STRIP=none` (or point at a debug build).

    tools/size-report.py dist/Domovoi.app "dist/Domovoi Server.app"
    tools/size-report.py --crates 30 target/aarch64-apple-darwin/release/domovoi-server

A path written `path=MB` is also checked against that budget: the report
fails (exit 1) when the executable is larger.
"""

import argparse
import collections
import os
import re
import subprocess
import sys

STD = {"core", "alloc", "std", "serde_json", "serde", "serde_core", "hashbrown", "futures_util", "futures_core"}
CRATE = re.compile(r"C(?:s[0-9a-zA-Z]{1,20}_)?(\d{1,3})(?=[a-zA-Z_])")
IDENT = re.compile(r"[a-zA-Z_][a-zA-Z0-9_]*")
SECTIONS = ("__text", "__const", "__cstring", "__eh_frame", "__unwind_info")


def mb(n):
    return f"{n / 1e6:7.2f} MB"


def run(*args):
    return subprocess.run(args, check=True, capture_output=True, text=True).stdout


def executable(path):
    """An app bundle's main executable, or the path itself."""
    if path.endswith(".app") and os.path.isdir(path):
        macos = os.path.join(path, "Contents", "MacOS")
        return os.path.join(macos, sorted(os.listdir(macos))[0])
    return path


def archs(binary):
    return run("lipo", "-archs", binary).split()


def segments(binary, arch):
    """(segment, size) and (section, size) of one architecture."""
    segs, secs = [], collections.Counter()
    for line in run("size", "-m", "-arch", arch, binary).splitlines():
        line = line.strip()
        if m := re.match(r"Segment (\S+): (\d+)", line):
            if m.group(1) != "__PAGEZERO":
                segs.append((m.group(1), int(m.group(2))))
        elif m := re.match(r"Section (\S+): (\d+)", line):
            if m.group(1) in SECTIONS:
                secs[m.group(1)] += int(m.group(2))
    return segs, secs


def crates(name):
    found = []
    for m in CRATE.finditer(name):
        k = int(m.group(1))
        s = name[m.end():m.end() + k]
        if len(s) == k and IDENT.fullmatch(s):
            found.append(s)
    return found


def owners(binary, arch):
    """Code bytes by owning crate (`[swift]`, `[C]` for the rest)."""
    rows = []
    for line in run("nm", "-arch", arch, "-n", binary).splitlines():
        parts = line.split(None, 2)
        if len(parts) == 3 and parts[1] in ("t", "T"):
            rows.append((int(parts[0], 16), parts[2]))
    rows.sort()
    out = collections.Counter()
    for (addr, name), (following, _) in zip(rows, rows[1:]):
        size = following - addr
        if not 0 <= size <= 5_000_000:
            continue
        if name.startswith(("_$s", "_$S")):
            out["[swift]"] += size
        elif name.startswith(("__R", "_R")):
            found = crates(name) or ["?"]
            user = [c for c in found if c not in STD]
            out[user[0] if user else found[0]] += size
        else:
            out["[C]"] += size
    return out


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("paths", nargs="+", help="Mach-O binaries or .app bundles, each optionally `=MB` (a budget)")
    parser.add_argument("--crates", type=int, metavar="N", default=0, help="also list the N largest crates by code")
    args = parser.parse_args()
    over = []
    for arg in args.paths:
        path, _, budget = arg.rpartition("=") if re.search(r"=\d+(\.\d+)?$", arg) else (arg, "", "")
        binary = executable(path)
        size = os.path.getsize(binary)
        note = f", budget {budget} MB" if budget else ""
        print(f"== {path}: {mb(size).strip()} ({' '.join(archs(binary))}){note}")
        if budget and size > float(budget) * 1e6:
            over.append(f"{path} is {mb(size).strip()}, over its {budget} MB budget")
        for arch in archs(binary):
            segs, secs = segments(binary, arch)
            print(f"  {arch}: " + ", ".join(f"{name} {mb(size).strip()}" for name, size in segs))
            print("    " + ", ".join(f"{name} {mb(size).strip()}" for name, size in secs.most_common()))
            if args.crates:
                by_crate = owners(binary, arch)
                if not by_crate:
                    print("    (stripped: no symbols to attribute code to)")
                for name, size in by_crate.most_common(args.crates):
                    print(f"    {mb(size)}  {name}")
    for message in over:
        print(f"size-report: {message}", file=sys.stderr)
    return 1 if over else 0


if __name__ == "__main__":
    sys.exit(main())

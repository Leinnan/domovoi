#!/usr/bin/env python3
"""Measure a build, keep it, and compare it with another.

    tools/perf.py save <label>             build the measurements of this checkout and keep them
    tools/perf.py run <label> [--only G]   run a kept build once and record the numbers
    tools/perf.py pairs <before> <after>   run two kept builds in alternation (default 3 pairs)
    tools/perf.py compare <before> <after> table of medians, spread and change
    tools/perf.py compare --self <label>   one build against itself: the noise on this machine

A kept build lives in `.reference/perf/<label>/`: the bench and test
executables, the server binary they start, the `fixtures` folder they read,
and one JSON file per run. It no longer depends on the checkout, so a build
saved before a change can be run again after it, in alternation with the new
one. That is the only comparison worth reading on a machine that is busy with
other work: single runs differ by more than most changes do.

`save` does not build in the checkout. It copies the checkout's own files
(tracked, and new ones git does not ignore) to `.reference/perf-build`, a
second tree with a target folder of its own, and builds there: the release
builds take a quarter of an hour each, and the checkout stays free to edit
and build meanwhile. Files keep their times, so only what changed is rebuilt.

Measurements are the divan benches (median per bench), and every line
`PERF <name> <value> <unit>` the ignored `perf_*` tests print. All of them are
costs: lower is better.

Groups (`--only`): bench, store, load, real. `real` needs a copy of a real
home (`--real-home`, default `.reference/perf-home` when it exists); it is
copied again before it is opened and never written.
"""

import argparse
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
KEPT = ROOT / ".reference" / "perf"
REAL_HOME = ROOT / ".reference" / "perf-home"
# Where `save` builds: a copy of the checkout with its own target folder.
BUILD = ROOT / ".reference" / "perf-build"

# (group, name the executable is kept under, cargo arguments that build it,
# the cargo target it is, arguments it is run with)
BENCH_ARGS = ["--bench"]
PERF_TEST_ARGS = ["perf_", "--ignored", "--nocapture", "--test-threads=1"]
BUILDS = [
    ("bench", "bench-frames", ["bench", "--no-run", "-p", "domovoi-rpc", "--bench", "frames"], ("frames", "bench"), BENCH_ARGS),
    ("bench", "bench-thread", ["bench", "--no-run", "-p", "domovoi-model", "--bench", "thread"], ("thread", "bench"), BENCH_ARGS),
    ("bench", "bench-view", ["bench", "--no-run", "-p", "domovoi_core", "--bench", "view"], ("view", "bench"), BENCH_ARGS),
    ("store", "server-unit", ["test", "--release", "--no-run", "-p", "domovoi-server", "--bin", "domovoi-server"], ("domovoi-server", "bin-test"), PERF_TEST_ARGS),
    ("load", "provider-turns", ["test", "--release", "--no-run", "-p", "domovoi-server", "--test", "provider_turns"], ("provider_turns", "test"), PERF_TEST_ARGS),
]
SERVER = "domovoi-server"
GROUPS = ["bench", "store", "load", "real"]
# Within `server-unit`: the real-home measurement is its own group.
REAL_TEST = "perf_real_home"

UNITS = {"ns": 1e-3, "µs": 1.0, "us": 1.0, "ms": 1e3, "s": 1e6}


def sh(args, **kw):
    return subprocess.run(args, cwd=ROOT, text=True, **kw)


def sync_build_tree():
    """Makes BUILD the checkout's own files as they are now, and nothing else
    beside its target folder."""
    listed = sh(["git", "ls-files", "-co", "--exclude-standard", "-z"], stdout=subprocess.PIPE).stdout.split("\0")
    wanted = {name for name in listed if name and (ROOT / name).is_file()}
    copied = 0
    for name in sorted(wanted):
        source, target = ROOT / name, BUILD / name
        before, now = (target.stat() if target.exists() else None), source.stat()
        if before and before.st_size == now.st_size and int(before.st_mtime) == int(now.st_mtime):
            continue
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
        copied += 1
    removed = 0
    for folder, folders, files in os.walk(BUILD):
        here = Path(folder)
        if here == BUILD:
            folders[:] = [f for f in folders if f != "target"]
        for file in files:
            if str((here / file).relative_to(BUILD)) not in wanted:
                (here / file).unlink()
                removed += 1
    print(f"perf: build tree {BUILD.relative_to(ROOT)}: {copied} file(s) copied, {removed} removed", flush=True)


def cargo_executables(args):
    """Runs cargo and returns {(target name, kind): executable path}."""
    done = subprocess.run(["cargo", *args, "--message-format=json"], cwd=BUILD, text=True, stdout=subprocess.PIPE)
    if done.returncode != 0:
        sys.exit(f"perf: `cargo {' '.join(args)}` failed")
    found = {}
    for line in done.stdout.splitlines():
        if not line.startswith("{"):
            continue
        message = json.loads(line)
        if message.get("reason") != "compiler-artifact" or not message.get("executable"):
            continue
        target = message["target"]
        kind = target["kind"][0]
        if kind == "bin" and message["profile"]["test"]:
            kind = "bin-test"
        found[(target["name"], kind)] = message["executable"]
    return found


def count_lines(pattern, paths, extensions):
    """Lines matching `pattern` in the checkout's own files under `paths`."""
    files = sh(["git", "ls-files", "-co", "--exclude-standard", "--", *paths], stdout=subprocess.PIPE).stdout.splitlines()
    matcher = re.compile(pattern) if pattern else None
    lines = hits = with_hit = 0
    for name in files:
        if not name.endswith(extensions):
            continue
        try:
            text = (ROOT / name).read_text(errors="replace")
        except OSError:
            continue
        lines += text.count("\n")
        if matcher:
            n = len(matcher.findall(text))
            hits += n
            with_hit += 1 if n else 0
    return lines, hits, with_hit


def static_measures():
    """What is read off the checkout, not timed."""
    source = (".rs",)
    rust_lines, _, _ = count_lines(None, ["apps", "crates"], source)
    _, _, value_files = count_lines(r"expect\(clippy::disallowed_types", ["apps", "crates"], source)
    _, t3_lines, _ = count_lines(r"(?i)t3 ?code|\bt3\b", ["apps", "crates", "conformance", "docs", "README.md", "justfile", ".github"], (".rs", ".swift", ".kt", ".md", ".ts", ".yml", ".toml", "justfile", ".py"))
    return {
        "code.rust_lines": (float(rust_lines), "count"),
        "code.files_opting_out_of_value_ban": (float(value_files), "count"),
        "code.t3_mentions": (float(t3_lines), "count"),
    }


def save(label):
    kept = KEPT / label
    if kept.exists():
        sys.exit(f"perf: {kept} exists; choose another label or remove it")
    sync_build_tree()
    # Read now: the checkout may be edited while the builds run.
    static = static_measures()
    commit = sh(["git", "rev-parse", "--short", "HEAD"], stdout=subprocess.PIPE).stdout.strip()
    dirty = bool(sh(["git", "status", "--porcelain"], stdout=subprocess.PIPE).stdout.strip())
    (kept / "bin").mkdir(parents=True)
    (kept / "runs").mkdir()
    for _, name, args, target, _ in BUILDS:
        print(f"perf: building {name}", flush=True)
        found = cargo_executables(args)
        if target not in found:
            sys.exit(f"perf: `cargo {' '.join(args)}` built no {target}")
        shutil.copy2(found[target], kept / "bin" / name)
        if name == "provider-turns":
            # The server the integration test was built to start.
            server = found.get((SERVER, "bin")) or str(BUILD / "target" / "release" / SERVER)
            shutil.copy2(server, kept / "bin" / SERVER)
    shutil.copytree(BUILD / "fixtures", kept / "fixtures")
    static["size.server_binary"] = (float((kept / "bin" / SERVER).stat().st_size), "bytes")
    (kept / "build.json").write_text(
        json.dumps(
            {
                "label": label,
                "commit": commit,
                "uncommitted_changes": dirty,
                "saved_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
                "rustc": subprocess.run(["rustc", "--version"], text=True, stdout=subprocess.PIPE).stdout.strip(),
                "profile": "release",
                "static": {k: {"value": v, "unit": u} for k, (v, u) in static.items()},
            },
            indent=1,
        )
    )
    print(f"perf: kept {label} ({commit}{' with uncommitted changes' if dirty else ''}) in {kept}")


def parse_divan(text, prefix):
    """{`prefix.bench[.arg]`: median in µs} from divan's table."""
    out = {}
    stack = []
    # A measurement printed while a bench ran split that bench's row in two.
    text = re.sub(r"PERF \S+ \S+ \S+\n", "", text)
    for line in text.splitlines():
        if "│" not in line or "fastest" in line:
            continue
        start = next((i for i, ch in enumerate(line) if ch not in " │├╰─"), None)
        if start is None:
            continue
        cells = line[start:].split("│")
        head = re.split(r"\s{2,}", cells[0].strip(), maxsplit=1)
        name = head[0]
        depth = start // 3
        del stack[max(depth - 1, 0):]
        stack.append(name)
        median = cells[2].strip() if len(cells) > 2 else ""
        match = re.fullmatch(r"([\d.]+)\s*(ns|µs|us|ms|s)", median)
        if match:
            out[".".join([prefix, *stack])] = (float(match.group(1)) * UNITS[match.group(2)], "us")
    return out


# A measurement's own line. It may follow other text on its line: the test
# harness's `test name ... `, or the start of a bench's row in divan's table.
PERF_LINE = re.compile(r"PERF (\S+) (-?[\d.]+(?:e-?\d+)?) (\S+)$", re.M)


def parse_perf_lines(text):
    return {match.group(1): (float(match.group(2)), match.group(3)) for match in PERF_LINE.finditer(text)}


def run(label, only=None, real_home=None):
    kept = KEPT / label
    if not (kept / "build.json").exists():
        sys.exit(f"perf: no kept build `{label}` (tools/perf.py save {label})")
    build = json.loads((kept / "build.json").read_text())
    env = dict(os.environ)
    env["DOMOVOI_FIXTURES"] = str(kept / "fixtures")
    env["DOMOVOI_SERVER_BIN"] = str(kept / "bin" / SERVER)
    env.pop("DOMOVOI_PERF_HOME", None)
    real_home = real_home or (REAL_HOME if REAL_HOME.exists() else None)
    measures = {k: (v["value"], v["unit"]) for k, v in build.get("static", {}).items()}
    failed = []
    started = time.time()
    load_before = os.getloadavg()[0]
    for group, name, _, _, args in BUILDS:
        if only and group not in only:
            continue
        run_args = list(args)
        if name == "server-unit":
            # The real home is its own step below.
            run_args += ["--skip", REAL_TEST]
        print(f"perf: {label}: {name}", flush=True)
        done = subprocess.run([str(kept / "bin" / name), *run_args], cwd=kept, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        (kept / "runs" / f"last-{name}.log").write_text(done.stdout)
        if done.returncode != 0:
            failed.append(name)
            print(done.stdout[-3000:], file=sys.stderr)
        measures.update(parse_divan(done.stdout, name.removeprefix("bench-")) if group == "bench" else {})
        measures.update(parse_perf_lines(done.stdout))
    if (not only or "real" in only) and real_home:
        print(f"perf: {label}: real home {real_home}", flush=True)
        real_env = dict(env, DOMOVOI_PERF_HOME=str(real_home))
        done = subprocess.run([str(kept / "bin" / "server-unit"), REAL_TEST, "--ignored", "--nocapture"], cwd=kept, env=real_env, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        (kept / "runs" / "last-real.log").write_text(done.stdout)
        if done.returncode != 0:
            failed.append("real")
            print(done.stdout[-3000:], file=sys.stderr)
        measures.update(parse_perf_lines(done.stdout))
    record = {
        "label": label,
        "at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "seconds": round(time.time() - started, 1),
        "load_average_before": load_before,
        "load_average_after": os.getloadavg()[0],
        "failed": failed,
        "measures": {k: {"value": v, "unit": u} for k, (v, u) in sorted(measures.items())},
    }
    path = kept / "runs" / f"{time.strftime('%Y%m%d-%H%M%S')}.json"
    path.write_text(json.dumps(record, indent=1))
    print(f"perf: {label}: {len(measures)} measures in {record['seconds']} s (load {load_before:.1f}) -> {path.relative_to(ROOT)}" + (f"; FAILED: {', '.join(failed)}" if failed else ""))
    return not failed


def runs_of(label):
    kept = KEPT / label
    runs = [json.loads(p.read_text()) for p in sorted((kept / "runs").glob("2*.json"))]
    if not runs:
        sys.exit(f"perf: `{label}` has no runs (tools/perf.py run {label})")
    return runs


def gathered(runs):
    """{measure: ([values], unit)} over runs."""
    out = {}
    for record in runs:
        for name, m in record["measures"].items():
            out.setdefault(name, ([], m["unit"]))[0].append(m["value"])
    return out


def shown(value, unit):
    if unit == "us":
        if value >= 1e6:
            return f"{value / 1e6:.2f} s"
        if value >= 1e3:
            return f"{value / 1e3:.2f} ms"
        return f"{value:.2f} µs"
    if unit == "bytes":
        if value >= 1e6:
            return f"{value / 1e6:.2f} MB"
        if value >= 1e3:
            return f"{value / 1e3:.1f} kB"
        return f"{value:.0f} B"
    if unit in ("ms", "s"):
        return f"{value:.1f} {unit}"
    return f"{value:g}"


def spread(values):
    """Half the range over the median: how far one run strays, as a share."""
    middle = statistics.median(values)
    if len(values) < 2 or middle == 0:
        return 0.0
    return (max(values) - min(values)) / 2 / abs(middle)


# A change smaller than this is not reported as one, whatever the runs' spread.
# Times: the same build measured against itself on the development machine
# (`compare --self`) differs by up to a fifth with three runs each. Bytes and
# counts are exact, so any change in them is one.
TIME_FLOOR = 0.20
EXACT_FLOOR = 0.005
TIMES = ("us", "ms", "s")


def compare(before_runs, after_runs, before_name, after_name, match=None, changed_only=False):
    before, after = gathered(before_runs), gathered(after_runs)
    names = sorted(set(before) | set(after))
    if match:
        names = [n for n in names if re.search(match, n)]
    rows = []
    for name in names:
        b, a = before.get(name), after.get(name)
        if not b or not a:
            only = b or a
            rows.append((name, shown(statistics.median(b[0]), b[1]) if b else "—", shown(statistics.median(a[0]), a[1]) if a else "—", "", "", "only in " + (before_name if b else after_name), only[1]))
            continue
        mb, ma = statistics.median(b[0]), statistics.median(a[0])
        noise = max(spread(b[0]), spread(a[0]), TIME_FLOOR if b[1] in TIMES else EXACT_FLOOR)
        if mb == 0:
            change, verdict = (0.0, "same") if ma == 0 else (float("inf"), "worse")
        else:
            change = (ma - mb) / abs(mb)
            verdict = "same" if abs(change) <= noise else ("better" if change < 0 else "worse")
        if changed_only and verdict == "same":
            continue
        rows.append((name, shown(mb, b[1]), shown(ma, a[1]), f"{change * 100:+.1f}%" if change != float("inf") else "new", f"±{noise * 100:.0f}%", verdict, b[1]))
    load = lambda runs: statistics.median(r["load_average_before"] for r in runs)
    print(f"`{before_name}`: {len(before_runs)} run(s), load {load(before_runs):.1f}; `{after_name}`: {len(after_runs)} run(s), load {load(after_runs):.1f}. Medians; a change within the runs' spread (at least ±{TIME_FLOOR * 100:.0f}% for a time, any change for bytes and counts) reads as the same.\n")
    print(f"| Measure | {before_name} | {after_name} | Change | Spread | |")
    print("|---|---|---|---|---|---|")
    for name, b, a, change, noise, verdict, _ in rows:
        print(f"| `{name}` | {b} | {a} | {change} | {noise} | {verdict} |")
    counts = {v: sum(1 for r in rows if r[5] == v) for v in ("better", "worse", "same")}
    print(f"\n{counts['better']} better, {counts['worse']} worse, {counts['same']} the same.")
    return counts["worse"]


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("save").add_argument("label")
    p = sub.add_parser("run")
    p.add_argument("label")
    p.add_argument("--only", action="append", choices=GROUPS)
    p.add_argument("--real-home", type=Path)
    p = sub.add_parser("pairs")
    p.add_argument("before")
    p.add_argument("after")
    p.add_argument("-n", type=int, default=3)
    p.add_argument("--only", action="append", choices=GROUPS)
    p.add_argument("--real-home", type=Path)
    p = sub.add_parser("compare")
    p.add_argument("before", nargs="?")
    p.add_argument("after", nargs="?")
    p.add_argument("--self", dest="itself")
    p.add_argument("--match", help="only measures matching this regular expression")
    p.add_argument("--changed", action="store_true", help="leave out the measures that are the same")
    args = parser.parse_args()

    if args.command == "save":
        save(args.label)
    elif args.command == "run":
        sys.exit(0 if run(args.label, args.only, args.real_home) else 1)
    elif args.command == "pairs":
        ok = True
        for _ in range(args.n):
            ok &= run(args.before, args.only, args.real_home)
            ok &= run(args.after, args.only, args.real_home)
        sys.exit(0 if ok else 1)
    elif args.itself:
        runs = runs_of(args.itself)
        if len(runs) < 2:
            sys.exit("perf: a build is compared with itself over at least two runs")
        # Alternate runs, as a pair of builds is run.
        compare(runs[0::2], runs[1::2], f"{args.itself} (odd runs)", f"{args.itself} (even runs)", args.match, args.changed)
    else:
        if not args.before or not args.after:
            parser.error("compare takes two labels, or --self <label>")
        compare(runs_of(args.before), runs_of(args.after), args.before, args.after, args.match, args.changed)


if __name__ == "__main__":
    main()

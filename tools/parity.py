#!/usr/bin/env python3
"""Feature parity across the native clients.

Every user-facing `Event` the core takes (`apps/domovoi-core/src/app.rs`, the
variants before the first `#[serde(skip)]`) and every field of the view model
(`apps/domovoi-core/src/view.rs`) should be used by each client: macOS, iOS,
Android, the Iced desktop app and the WinUI app for Windows. A name a client
never mentions is a feature it lacks. Differences that are on purpose are listed, with the reason, in
`tools/parity-allowlist.toml`. The check fails on any other gap, and on an
allowlist entry that no longer holds, so the list stays true.

    tools/parity.py            # fails (exit 1) on an unlisted gap
    tools/parity.py --matrix   # every event and field, with counts per client

The match is by name (`.selectThread` in Swift, `Event.SelectThread` in
Kotlin, `Event::SelectThread` in Rust; `.fieldName` or `.field_name`), so it
finds what a client never references, not whether it uses it well.
Generated bindings, builds, tests and snapshot fixtures are left out.
"""

import argparse
import pathlib
import re
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent
APPS = ROOT / "apps"
CORE = APPS / "domovoi-core" / "src"

APPLE = APPS / "domovoi-apple"
CLIENTS = {
    "macos": ("swift", [APPLE / "Common", APPLE / "macOS"]),
    "ios": ("swift", [APPLE / "Common", APPLE / "iOS", APPLE / "Activity", APPLE / "Widgets", APPLE / "ShareExtension"]),
    "android": ("kotlin", [APPS / "domovoi-android" / "app" / "src" / "main"]),
    "desktop": ("rust", [APPS / "domovoi-iced" / "src"]),
    "windows": ("rust", [APPS / "domovoi-windows" / "src"]),
}
# Clients still being built: in `--matrix` and counted, but their gaps don't
# fail the check yet. Drop one from here once it reaches the others, and the
# check holds it to them. None is being built now.
WIP_CLIENTS = set()
EXTENSIONS = {"swift": ".swift", "kotlin": ".kt", "rust": ".rs"}
# Fixtures: they build views and send events to draw a screen, not as a feature.
SKIP_FILES = {"Snapshots.swift", "IOSSnapshots.swift", "ui_tests.rs"}
SKIP_DIRS = {"generated", "build", "test", "tests"}


def camel(name):
    parts = name.split("_")
    return parts[0] + "".join(p[:1].upper() + p[1:] for p in parts[1:])


def lower_first(name):
    return name[:1].lower() + name[1:]


def core_events():
    text = (CORE / "app.rs").read_text()
    body = text[text.index("pub enum Event {"):]
    body = body[: body.index("#[serde(skip)]")]
    return sorted(set(re.findall(r"^    ([A-Z]\w*)\b", body, re.M)))


def core_fields():
    fields, owner = [], None
    for line in (CORE / "view.rs").read_text().splitlines():
        m = re.match(r"pub (?:struct|enum) (\w+)", line)
        if m:
            owner = m.group(1)
            continue
        m = re.match(r"    pub (\w+):", line)
        if m and owner:
            fields.append((owner, m.group(1)))
    return fields


def client_source(lang, dirs):
    texts = []
    for d in dirs:
        for p in sorted(d.rglob("*" + EXTENSIONS[lang])):
            if p.name in SKIP_FILES or SKIP_DIRS & set(p.relative_to(d).parts):
                continue
            texts.append(p.read_text(errors="ignore"))
    text = "\n".join(texts)
    if lang == "swift":
        # Arms that map the generated enum: not a use of the event.
        text = re.sub(r"\b(case|return) \.[a-z]\w*", "", text)
    return text


def event_pattern(lang, event):
    if lang == "swift":
        return r"\." + lower_first(event) + r"\b"
    if lang == "kotlin":
        return r"\bEvent\." + event + r"\b"
    return r"\bEvent::" + event + r"\b"


def field_pattern(lang, field):
    return r"\." + (field if lang == "rust" else camel(field)) + r"\b"


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--matrix", action="store_true", help="print every event and field with counts per client")
    args = ap.parse_args()

    allow = tomllib.loads((ROOT / "tools" / "parity-allowlist.toml").read_text())
    sources = {name: (lang, client_source(lang, dirs)) for name, (lang, dirs) in CLIENTS.items()}
    names = list(CLIENTS)

    rows = [("event", e, {c: len(re.findall(event_pattern(lang, e), src)) for c, (lang, src) in sources.items()}) for e in core_events()]
    rows += [("field", f"{o}.{f}", {c: len(re.findall(field_pattern(lang, f), src)) for c, (lang, src) in sources.items()}) for o, f in core_fields()]

    if args.matrix:
        print(f"{'':6} {'name':48}" + "".join(f"{c:>9}" for c in names))
        for kind, name, counts in rows:
            print(f"{kind:6} {name:48}" + "".join(f"{counts[c]:>9}" for c in names))

    problems = []
    seen = set()
    for kind, name, counts in rows:
        table = allow.get("events" if kind == "event" else "fields", {})
        entry = table.get(name)
        if entry is not None:
            seen.add((kind, name))
        allowed = set(entry.get("missing", [])) if entry else set()
        if "*" in allowed:
            allowed = set(names)
        for c in names:
            if c in WIP_CLIENTS:
                continue
            if counts[c] == 0 and c not in allowed:
                problems.append(f"{kind} {name}: missing in {c}")
            elif counts[c] > 0 and c in allowed and "*" not in (entry or {}).get("missing", []):
                problems.append(f"{kind} {name}: allowlisted as missing in {c}, but {c} uses it now (remove it from the allowlist)")

    known = {("event", n) for n in allow.get("events", {})} | {("field", n) for n in allow.get("fields", {})}
    for kind, name in sorted(known - seen):
        problems.append(f"{kind} {name}: in the allowlist but not in the core any more")

    for p in problems:
        print(p)
    for c in sorted(WIP_CLIENTS):
        used = sum(1 for _, _, counts in rows if counts[c] > 0)
        print(f"parity: {c} is in progress: {used} of {len(rows)} events and fields so far")
    total = len(rows)
    if problems:
        print(f"parity: {len(problems)} gap(s) across {total} events and fields (tools/parity-allowlist.toml lists the intended ones)")
        return 1
    print(f"parity: {total} events and fields, every client up to date")
    return 0


if __name__ == "__main__":
    sys.exit(main())

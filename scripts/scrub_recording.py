#!/usr/bin/env python3
"""Turn a recorded session (and provider logs) into a committable fixture.

Scrubs account identifiers, credentials, and machine-specific paths. Usage
(`fixtures/native/README.md` says how a session is recorded):

    scripts/scrub_recording.py --tap rec.ndjson --home /Users/me --out fixtures/native/<name>

Run scripts/check_fixtures.py afterwards; it fails if anything sensitive remains.
"""
import argparse
import json
import pathlib
import re

SENSITIVE_KEY = re.compile(r"(token|secret|credential|password|authorization|cookie|dpop|proof|signature|apikey|api_key)", re.I)
EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")
BEARER = re.compile(r"(Bearer|DPoP)\s+[A-Za-z0-9._~+/=-]+")
QUERY_SECRET = re.compile(r"((?:token|ticket|dpop|clientDevice)=)[^&#\s\"]+")
# Keys whose values identify the account or machine rather than protocol shape.
IDENTITY_KEYS = {"email", "workspaceId", "accountId", "account", "login", "username", "organizationId", "userId", "anonymousId", "clientDevice"}
DROP_HTTP_PREFIXES = ("/api/observability/",)


IDENTITY_IN_TEXT = re.compile(
    r'(\\*"(?:' + "|".join(sorted(IDENTITY_KEYS)) + r')\\*"\s*:\s*\\*")[^"\\]+'
)


EXTRA_REDACTIONS: list[tuple[str, str]] = []


def scrub_str(s: str, home: str) -> str:
    s = s.replace(home, "/HOME")
    for needle, replacement in EXTRA_REDACTIONS:
        s = s.replace(needle, replacement)
    s = IDENTITY_IN_TEXT.sub(lambda m: f"{m.group(1)}<redacted>", s)
    s = EMAIL.sub("user@example.com", s)
    s = BEARER.sub(lambda m: f"{m.group(1)} <redacted>", s)
    s = QUERY_SECRET.sub(lambda m: f"{m.group(1)}<redacted>", s)
    return s


def redact_all(v):
    """Redacts every string under an identity key (e.g. Effect Option `{"_tag":"Some","value":..}`)."""
    if isinstance(v, dict):
        return {k: (x if k == "_tag" else redact_all(x)) for k, x in v.items()}
    if isinstance(v, list):
        return [redact_all(x) for x in v]
    return "<redacted>" if isinstance(v, str) else v


def scrub(v, home: str, key: str | None = None):
    if key in IDENTITY_KEYS and isinstance(v, (dict, list)):
        return redact_all(v)
    if isinstance(v, dict):
        return {k: scrub(x, home, k) for k, x in v.items()}
    if isinstance(v, list):
        return [scrub(x, home, key) for x in v]
    if isinstance(v, str):
        if key and (key in IDENTITY_KEYS or (SENSITIVE_KEY.search(key) and not key.endswith(("Count", "Usage", "Tokens")))):
            return "<redacted>"
        # JSON embedded in strings (e.g. lifecycle payloads) gets scrubbed structurally too.
        if len(v) > 1 and v[0] in "{[":
            try:
                return json.dumps(scrub(json.loads(v), home), separators=(",", ":"))
            except ValueError:
                pass
        return scrub_str(v, home)
    return v


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--tap", required=True)
    ap.add_argument("--raw", action="append", default=[], metavar="NAME=PATH",
                    help="raw stdio capture to scrub line by line into <out>/NAME")
    ap.add_argument("--provider-log", action="append", default=[])
    ap.add_argument("--home", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--redact", action="append", default=[], metavar="LITERAL=REPLACEMENT",
                    help="extra machine-specific literals (hostnames, usernames) to replace")
    a = ap.parse_args()
    for item in a.redact:
        needle, _, replacement = item.partition("=")
        EXTRA_REDACTIONS.append((needle, replacement or "<redacted>"))
    out = pathlib.Path(a.out)
    out.mkdir(parents=True, exist_ok=True)

    kept = 0
    pending: dict[tuple, str] = {}
    with open(a.tap) as src, open(out / "wire.ndjson", "w") as dst:
        for line in src:
            v = json.loads(line)
            # Rejoin t3code frameParts logged by older taps as raw string parts.
            if v.get("kind") == "ws" and isinstance(v.get("frame"), str):
                key = (v.get("conn"), v.get("dir"))
                if v["frame"].startswith("\x1e"):
                    pending[key] = pending.get(key, "") + v["frame"][1:]
                    continue
                if key in pending:
                    whole = pending.pop(key) + v["frame"]
                    try:
                        v["frame"] = json.loads(whole)
                    except ValueError:
                        v["frame"] = whole
            if v.get("kind") == "http" and v.get("path", "").startswith(DROP_HTTP_PREFIXES):
                continue
            v.pop("setCookie", None) if v.get("kind") != "http" else v.__setitem__("setCookie", ["<redacted>"] * len(v.get("setCookie", [])))
            dst.write(json.dumps(scrub(v, a.home), separators=(",", ":")) + "\n")
            kept += 1

    native = 0
    with open(out / "provider-native.ndjson", "w") as dst:
        for path in a.provider_log:
            for line in open(path):
                i = line.find("{")
                if i < 0:
                    continue
                ts = line[1:line.find("]")] if line.startswith("[") else None
                try:
                    v = json.loads(line[i:])
                except ValueError:
                    continue
                v["loggedAt"] = ts
                dst.write(json.dumps(scrub(v, a.home), separators=(",", ":")) + "\n")
                native += 1
    # Raw stdio captures (claude-tap): NAME=PATH, scrubbed line by line.
    for item in a.raw:
        name, _, path = item.partition("=")
        with open(path) as src, open(out / name, "w") as dst:
            for line in src:
                try:
                    dst.write(json.dumps(scrub(json.loads(line), a.home), separators=(",", ":")) + "\n")
                except ValueError:
                    dst.write(scrub_str(line.rstrip("\n"), a.home) + "\n")
    print(f"wrote {kept} wire records, {native} provider records, {len(a.raw)} raw files to {out}")


if __name__ == "__main__":
    main()

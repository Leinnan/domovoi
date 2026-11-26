# The stand-in for the Claude CLI: keeps the MCP configuration it is started with, then fails at once.
mkdir -p "$DIR/out" "$DIR/mcp-out"
cat > "$DIR/bin/claude" <<'EOF'
#!/bin/bash
here="$(cd "$(dirname "$0")/.." && pwd)"
for a in "$@"; do case "$a" in --mcp-config=*) printf '%s\n' "${a#--mcp-config=}" >> "$here/out/mcp-config.jsonl";; esac; done
exit 1
EOF
chmod +x "$DIR/bin/claude"
# mcp <name> <body> [token=none|token=bad] [save=VAR:/json/pointer] [wait=seconds]
# Posts <body> (a JSON-RPC message; @VAR@ stands for a saved value) to the MCP route as the agent
# of the first thread that was launched, and writes the status and the answer to mcp-out/<name>.json.
cat > "$DIR/bin/mcp" <<'EOF'
#!/usr/bin/python3
import json, os, sys, time, urllib.request, urllib.error
d = os.environ["DIR"]
name, body, opts = sys.argv[1], sys.argv[2], dict(o.split("=", 1) for o in sys.argv[3:])
time.sleep(float(opts.get("wait", "0")))
server = json.loads(open(d + "/out/mcp-config.jsonl").readline())["mcpServers"]["domovoi"]
headers = {"content-type": "application/json"}
if opts.get("token") == "bad": headers["Authorization"] = "Bearer domovoi_mcp_00000000000000000000000000000000"
elif opts.get("token") != "none": headers.update(server["headers"])
saved = d + "/mcp-out/.vars/"
os.makedirs(saved, exist_ok=True)
for var in os.listdir(saved): body = body.replace("@" + var + "@", open(saved + var).read())
try:
    with urllib.request.urlopen(urllib.request.Request(server["url"], data=body.encode(), headers=headers, method="POST")) as res:
        status, text = res.status, res.read().decode()
except urllib.error.HTTPError as e:
    status, text = e.code, e.read().decode()
try: answer = json.loads(text)
except ValueError: answer = text
if "save" in opts:
    var, pointer = opts["save"].split(":", 1)
    found = answer
    for key in pointer.strip("/").split("/"): found = found[int(key)] if isinstance(found, list) else found[key]
    open(saved + var, "w").write(found)
written = json.dumps({"status": status, "answer": answer}, sort_keys=True, indent=1)
for folder in sorted({os.path.realpath(d), d}, key=len, reverse=True): written = written.replace(folder, "<dir>")
open(d + "/mcp-out/" + name + ".json", "w").write(written)
EOF
chmod +x "$DIR/bin/mcp"

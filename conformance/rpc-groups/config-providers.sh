mkdir -p home/userdata state
cat > home/userdata/claude-slash-commands.json <<'E'
[{"name":"review"},{"name":"compact"},{"name":"deep-research"}]
E
cat > bin/claude <<'E'
#!/bin/sh
state="$(dirname "$0")/../state"
case "$*" in
  *--version*) cat "$state/claude-version" 2>/dev/null || exit 1;;
  *"auth status"*) cat "$state/claude-auth" 2>/dev/null || exit 1;;
  *) exit 1;;
esac
E
cat > bin/codex <<'E'
#!/bin/sh
state="$(dirname "$0")/../state"
case "$*" in
  *--version*) cat "$state/codex-version" 2>/dev/null || exit 1;;
  *"login status"*) cat "$state/codex-auth" 2>/dev/null || exit 1;;
  *) exit 1;;
esac
E
cat > bin/pi <<'E'
#!/bin/sh
state="$(dirname "$0")/../state"
case "$1" in
  --version) cat "$state/pi-version" 2>/dev/null || exit 1;;
  --mode)
    [ -f "$state/pi-rpc" ] || exit 1
    exec /usr/bin/python3 "$(dirname "$0")/pi-rpc.py" "$state" "$@";;
  auth) cat "$state/pi-auth-check" 2>/dev/null || exit 1;;
  *) exit 1;;
esac
E
cat > bin/pi-rpc.py <<'E'
import json, os, sys
state, args = sys.argv[1], sys.argv[2:]
def read(name, default):
    p = os.path.join(state, name)
    return json.load(open(p)) if os.path.exists(p) else default
# Text generation's probe runs without extensions: models an extension registers are missing.
models = read("pi-models-bare" if "--no-extensions" in args else "pi-models", {"models": []})
for line in sys.stdin:
    m = json.loads(line)
    t = m.get("type")
    data = {"get_state": read("pi-state", {"thinkingLevel": "off", "isStreaming": False}), "get_available_models": models, "get_commands": read("pi-commands", None)}.get(t)
    r = {"type": "response", "id": m.get("id"), "command": t}
    if data is None:
        r.update(success=False, error="Unknown command: " + str(t))
    else:
        r.update(success=True, data=data)
    sys.stdout.write(json.dumps(r) + "\n"); sys.stdout.flush()
E
cat > bin/opencode <<'E'
#!/bin/sh
state="$(dirname "$0")/../state"
case "$1" in
  --version) cat "$state/opencode-version" 2>/dev/null || exit 1;;
  serve)
    [ -f "$state/opencode-serve" ] || exit 1
    exec /usr/bin/python3 "$(dirname "$0")/opencode-serve.py" "$5" "$state";;
  *) exit 1;;
esac
E
cat > bin/opencode-serve.py <<'E'
import http.server, json, os, sys
port, state = int(sys.argv[1]), sys.argv[2]
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        if self.path == "/global/health":
            body = {"healthy": True}
        elif self.path == "/api/model":
            p = os.path.join(state, "opencode-models")
            if not os.path.exists(p):
                self.send_response(500); self.send_header("content-type", "application/json"); self.end_headers()
                self.wfile.write(json.dumps({"message": "No providers are connected."}).encode()); return
            body = json.load(open(p))
        elif self.path == "/api/model/default":
            p = os.path.join(state, "opencode-default")
            if not os.path.exists(p):
                self.send_response(404); self.end_headers(); return
            body = json.load(open(p))
        elif self.path == "/api/event":
            self.send_response(200); self.send_header("content-type", "text/event-stream"); self.end_headers()
            try:
                import time
                while True:
                    time.sleep(1)
            except Exception:
                return
        else:
            self.send_response(404); self.end_headers(); return
        data = json.dumps(body).encode()
        self.send_response(200); self.send_header("content-type", "application/json"); self.send_header("content-length", str(len(data))); self.end_headers()
        self.wfile.write(data)
http.server.ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
E
chmod +x bin/claude bin/codex bin/opencode bin/pi
echo "2.1.319 (Claude Code)" > state/claude-version
echo '{"loggedIn": true, "authMethod": "claude.ai", "subscriptionType": "max"}' > state/claude-auth
echo "codex-cli 0.166.0" > state/codex-version
echo "Logged in using ChatGPT" > state/codex-auth
echo "1.9.4" > state/opencode-version
echo "0.84.3" > state/pi-version

# Stand-ins for `lsof` and `ps`, so the listeners local-server discovery sees
# are the same every run (pids above any real one, so nothing real is ever
# signaled), and a small web server on two fixed loopback ports for the
# configured-URL probe. The web server ends when this folder goes (or after
# ten minutes), and waits for the ports if the one of a run before still has them.

cat > "$DIR/bin/lsof" <<'SH'
#!/bin/bash
# What listens, as `lsof -F pcn` prints it.
here="$(cd "$(dirname "$0")/.." && pwd)"
[ -e "$here/lsof.txt" ] && cat "$here/lsof.txt"
exit 0
SH

cat > "$DIR/bin/ps" <<'SH'
#!/bin/bash
# `ps -A -o pid=,ppid=,pgid=`: the real table plus the rows of ps-rows.txt, where
# SHELL is each other child of the server (its terminal shells) and SERVERGROUP
# the server's process group. `ps -E ... -p <pids>`: the lines of ps-env.txt.
here="$(cd "$(dirname "$0")/.." && pwd)"
case " $* " in
  *" -E "*) [ -e "$here/ps-env.txt" ] && cat "$here/ps-env.txt"; exit 0 ;;
esac
[ -e "$here/ps-fail" ] && exit 1
table="$(/bin/ps -A -o pid=,ppid=,pgid=)"
echo "$table"
group="$(echo "$table" | awk -v s="$PPID" '$1==s {print $3}')"
if [ -e "$here/ps-rows.txt" ]; then
  shells="$(echo "$table" | awk -v s="$PPID" -v me="$$" '$2==s && $1!=me {print $1}')"
  for shell in ${shells:-0}; do
    sed -e "s/SHELL/$shell/g" -e "s/SERVERGROUP/$group/g" "$here/ps-rows.txt"
  done
fi
exit 0
SH
chmod +x "$DIR/bin/lsof" "$DIR/bin/ps"

cat > "$DIR/httpd.py" <<'PY'
import http.server, os, sys, threading, time

class Page(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"<html><body>ok</body></html>"
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *args):
        pass

folder = sys.argv[1]
started = time.time()
for port in (47903, 47907):
    # The server of a run just before may still hold the port for a moment.
    while True:
        try:
            server = http.server.ThreadingHTTPServer(("127.0.0.1", port), Page)
            break
        except OSError:
            if time.time() - started > 20:
                raise
            time.sleep(0.1)
    threading.Thread(target=server.serve_forever, daemon=True).start()
open(os.path.join(folder, "httpd.ready"), "w").close()
while os.path.exists(folder) and time.time() - started < 600:
    time.sleep(0.2)
PY
nohup python3 "$DIR/httpd.py" "$DIR" >/dev/null 2>&1 &
for _ in $(seq 1 250); do
  [ -e "$DIR/httpd.ready" ] && break
  sleep 0.1
done
[ -e "$DIR/httpd.ready" ]

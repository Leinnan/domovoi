# Stand-ins for `curl` (the OTLP endpoint check and the model list of a text
# generator) and for 1Password's `op`, the settings those calls read, and a
# server log newer than the server's own for the triage prompt.

cat > "$DIR/bin/curl" <<'SH'
#!/bin/bash
# Answers by the URL (the last argument); for an OTLP export, by its body.
url="${@: -1}"
has() { for arg in "${ARGS[@]}"; do [ "$arg" = "$1" ] && return 0; done; return 1; }
ARGS=("$@")
case "$url" in
  http://otlp.test/by-signal)
    case "$*" in
      *resourceSpans*) printf 401 ;;
      *resourceMetrics*) printf 402 ;;
      *resourceLogs*) printf 403 ;;
      *) printf 400 ;;
    esac ;;
  http://otlp.test/unavailable) printf 503 ;;
  http://otlp.test/zero) printf 000 ;;
  http://otlp.test/garbage) printf 'not a status' ;;
  http://otlp.test/timeout) exit 28 ;;
  http://otlp.test/refused) exit 7 ;;
  http://gen.test/v1/models)
    has "authorization: Bearer k-openai" || exit 22
    printf '{"object":"list","data":[{"id":"m-1","name":"Model One"},{"id":"m-2"},{"id":"m-3","display_name":"Three","name":"three"},{"name":"no id"}]}' ;;
  https://api.anthropic.com/v1/models)
    has "x-api-key: k-anthropic" && has "anthropic-version: 2023-06-01" || exit 22
    printf '{"data":[{"id":"claude-x","display_name":"Claude X"}],"has_more":false}' ;;
  http://anthropic.test/v1/models) printf '{"data":[]}' ;;
  http://nodata.test/models) printf '{"object":"list"}' ;;
  http://notjson.test/models) printf 'not json' ;;
  *) exit 6 ;;
esac
SH

cat > "$DIR/bin/op" <<'SH'
#!/bin/bash
# `op --cache=false read --no-newline [--account=<account>] <reference>`.
reference="${@: -1}"
case "$reference" in
  op://vault/item/ok) printf 's3cret' ;;
  op://vault/item/account) case "$*" in *"--account=acct-1"*) printf 's3cret' ;; *) exit 1 ;; esac ;;
  op://vault/item/empty) printf '  \n' ;;
  *) echo "[ERROR] no such item" >&2; exit 1 ;;
esac
SH
chmod +x "$DIR/bin/curl" "$DIR/bin/op"

mkdir -p "$DIR/home/userdata/logs"
cat > "$DIR/home/userdata/settings.json" <<'JSON'
{
  "secretProviders": {
    "op-main": { "kind": "onepassword", "label": "Team vault", "driver": "onepassword", "account": "acct-1" },
    "1password": { "kind": "onepassword", "label": "Personal" },
    "onepassword-cli": {},
    "env": { "kind": "env", "label": "Environment", "driver": "env" }
  },
  "textGenerators": {
    "openai-like": { "kind": "openaiCompatible", "baseUrl": "http://gen.test/v1/", "apiKey": "k-openai" },
    "claude": { "kind": "anthropic", "baseUrl": "", "apiKey": "k-anthropic" },
    "claude-proxy": { "kind": "anthropic", "baseUrl": "http://anthropic.test", "apiKey": "k" },
    "by-reference": { "kind": "openaiCompatible", "baseUrl": "http://gen.test/v1", "apiKey": { "provider": "op-main", "reference": "op://vault/item/ok" } },
    "no-data": { "kind": "openaiCompatible", "baseUrl": "http://nodata.test", "apiKey": "k" },
    "not-json": { "kind": "openaiCompatible", "baseUrl": "http://notjson.test", "apiKey": "k" },
    "down": { "kind": "openaiCompatible", "baseUrl": "http://down.test", "apiKey": "k" }
  }
}
JSON
# The triage prompt quotes the last 40 lines of the newest log: 50 lines, dated ahead of the server's own.
for i in $(seq 1 50); do echo "line $i of the planted log"; done > "$DIR/home/userdata/logs/planted.log"
touch -t 203501010000 "$DIR/home/userdata/logs/planted.log"

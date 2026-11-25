# A Claude Code installed with npm under npm/ (bin/claude links into the
# package), and an npm there that "updates" it, or fails once state/npm-fail
# exists. A Codex that no package manager installed.
mkdir -p state npm/bin npm/lib/node_modules/@anthropic-ai/claude-code
echo "2.1.0 (Claude Code)" > state/claude-version
cat > npm/lib/node_modules/@anthropic-ai/claude-code/cli.js <<E
#!/bin/sh
case "\$*" in
  *--version*) cat "$DIR/state/claude-version";;
  *"auth status"*) echo '{"loggedIn": true, "authMethod": "claude.ai", "subscriptionType": "max"}';;
  *) exit 1;;
esac
E
cat > npm/bin/npm <<E
#!/bin/sh
echo "\$@" > "$DIR/state/npm-args"
if [ -f "$DIR/state/npm-fail" ]; then echo "npm error code E404" >&2; echo "npm error 404 Not Found" >&2; exit 1; fi
echo "downloading 50%" >&2
echo "2.2.0 (Claude Code)" > "$DIR/state/claude-version"
echo "changed 1 package in 1s"
E
cat > bin/codex <<'E'
#!/bin/sh
case "$*" in
  *--version*) echo "codex-cli 0.166.0";;
  *"login status"*) echo "Logged in using ChatGPT";;
  *) exit 1;;
esac
E
chmod +x npm/lib/node_modules/@anthropic-ai/claude-code/cli.js npm/bin/npm bin/codex
ln -s "$DIR/npm/lib/node_modules/@anthropic-ai/claude-code/cli.js" bin/claude

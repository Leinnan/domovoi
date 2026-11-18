# Where a managed copy of the relay client would be, by this machine's
# platform and architecture as Node names them.
case "$(uname -s)" in Darwin) os=darwin ;; *) os=linux ;; esac
case "$(uname -m)" in arm64|aarch64) arch=arm64 ;; *) arch=x64 ;; esac
echo "home/relay-client/2026.10.4/$os-$arch" > "$DIR/managed-dir"

# Writes a stand-in `cloudflared` that reports the version given.
cat > "$DIR/make-cloudflared" <<'SH'
#!/bin/bash
mkdir -p "$(dirname "$1")"
printf '#!/bin/bash\necho "cloudflared version %s (built 2026-01-01-0000 UTC)"\n' "$2" > "$1"
chmod +x "$1"
SH
chmod +x "$DIR/make-cloudflared"

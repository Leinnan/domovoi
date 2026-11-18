# Writes a stand-in `cloudflared` that reports the version given.
cat > "$DIR/make-cloudflared" <<'SH'
#!/bin/bash
mkdir -p "$(dirname "$1")"
printf '#!/bin/bash\necho "cloudflared version %s (built 2026-01-01-0000 UTC)"\n' "$2" > "$1"
chmod +x "$1"
SH
chmod +x "$DIR/make-cloudflared"
# One on PATH, which the override must win over.
"$DIR/make-cloudflared" "$DIR/bin/cloudflared" 2025.12.0

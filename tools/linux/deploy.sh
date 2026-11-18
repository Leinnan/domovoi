#!/usr/bin/env bash
# Builds a headless domovoi-server for Linux on this Mac (tools/cross/linux.sh)
# and installs it on a Linux machine over ssh, as a systemd user service:
# ~/.local/bin/domovoi-server, ~/.config/systemd/user/domovoi-server.service,
# state in ~/.domovoi. Waits until the server is up and prints its remote
# access status. Usage: deploy.sh user@host
set -euo pipefail
host="${1:?usage: deploy.sh user@host}"
cd "$(dirname "$0")/../.."
bin=$(tools/cross/linux.sh)
dest=.local/bin/domovoi-server

echo "Copying $(du -h "$bin" | cut -f1) to $host:~/$dest" >&2
ssh "$host" 'mkdir -p ~/.local/bin'
scp -q "$bin" "$host:$dest.new"
# Renaming over the running binary is safe; the restart picks up the new one.
ssh "$host" "chmod +x ~/$dest.new && mv -f ~/$dest.new ~/$dest && ~/$dest service install"

for _ in $(seq 30); do
  status=$(ssh "$host" "~/$dest remote status" 2>&1 || true)
  case "$status" in
    *"Remote access: on (relay"* | *"Remote access: on (local network only)"* | *"Remote access: off"*) echo "$status"; exit 0 ;;
    *"Remote access: failed"*) echo "$status" >&2; exit 1 ;;
  esac
  sleep 2
done
echo "$status" >&2
echo "the server did not come up within a minute; \`just linux-logs $host\` shows why" >&2
exit 1

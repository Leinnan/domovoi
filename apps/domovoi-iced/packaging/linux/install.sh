#!/usr/bin/env sh
# Install Domovoi Desktop for this user (or under PREFIX): the binary,
# desktop entry (including domovoi:// handler), and icon. Run from the unpacked tarball.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
prefix=${PREFIX:-$HOME/.local}
install -Dm755 "$here/bin/domovoi-desktop" "$prefix/bin/domovoi-desktop"
install -Dm644 "$here/share/applications/domovoi.desktop" "$prefix/share/applications/domovoi.desktop"
install -Dm644 "$here/share/icons/hicolor/256x256/apps/domovoi.png" "$prefix/share/icons/hicolor/256x256/apps/domovoi.png"
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$prefix/share/applications" || true
# Pairing links open here (the entry lists the scheme; this makes it the default).
command -v xdg-mime >/dev/null 2>&1 && xdg-mime default domovoi.desktop x-scheme-handler/domovoi || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$prefix/share/icons/hicolor" || true
echo "installed to $prefix (make sure $prefix/bin is on PATH)"

source "$(dirname "${BASH_SOURCE[0]}")/vcs-common.sh"

# Editors: one that starts, and files that are found but cannot be started
# (so the failure shows the command line each launch style builds).
printf '#!/bin/sh\necho "$@" >> "%s/launched.log"\n' "$DIR" > "$DIR/bin/cursor"
chmod +x "$DIR/bin/cursor"
for name in code zeditor kiro idea; do echo "not a program" > "$DIR/bin/$name"; done
# Finder is never opened: `open` is this stand-in.
printf '#!/bin/sh\necho "open $@" >> "%s/launched.log"\n' "$DIR" > "$DIR/bin/open"
chmod +x "$DIR/bin/open"

# Tools `server.discoverSourceControl` looks for.
printf '#!/bin/sh\necho "gh version 2.99.0 (2026-01-01)"\necho "https://github.com/cli/cli/releases/tag/v2.99.0"\n' > "$DIR/bin/gh"
printf '#!/bin/sh\necho "jj 0.99.0"\n' > "$DIR/bin/jj"
chmod +x "$DIR/bin/gh" "$DIR/bin/jj"

mkdir -p "$DIR/files/folder"
echo "text" > "$DIR/files/a.rs"

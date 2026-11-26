source "$(dirname "${BASH_SOURCE[0]}")/vcs-common.sh"

# Tools `server.discoverSourceControl` looks for.
printf '#!/bin/sh\necho "gh version 2.99.0 (2026-01-01)"\necho "https://github.com/cli/cli/releases/tag/v2.99.0"\n' > "$DIR/bin/gh"
printf '#!/bin/sh\necho "jj 0.99.0"\n' > "$DIR/bin/jj"
chmod +x "$DIR/bin/gh" "$DIR/bin/jj"

mkdir -p "$DIR/files/folder"
echo "text" > "$DIR/files/a.rs"

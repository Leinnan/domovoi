# Stand-ins for `gh` and `ssh`, and the repository clones are made of (see repos-lib.sh).
source "$(dirname "${BASH_SOURCE[0]}")/repos-lib.sh"

# Destinations a clone must refuse, and one it may use.
mkdir -p "$DIR/clones/full" "$DIR/clones/empty"
echo "kept" > "$DIR/clones/full/notes.txt"
echo "a file" > "$DIR/clones/afile"

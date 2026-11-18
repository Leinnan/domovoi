# Sourced by each `prs-*.sh` in the group's folder (`$DIR`): puts the stand-in
# `gh` and its canned answers in place and makes the checkouts the group's
# projects are on. Nothing here reaches a network: the remotes are only names.

FIXTURES="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

cp -R "$FIXTURES/data" "$DIR/gh-data"
: > "$DIR/gh-data/rules"
mkdir -p "$DIR/tools" "$DIR/repos"
cp "$FIXTURES/gh-rule" "$DIR/bin/gh-rule"
cp "$FIXTURES/gh" "$DIR/tools/gh"
cp "$FIXTURES/codex" "$DIR/tools/codex"
chmod +x "$DIR/bin/gh-rule" "$DIR/tools/gh" "$DIR/tools/codex"

printf '[user]\n\tname = t\n\temail = t@t\n[init]\n\tdefaultBranch = main\n' > "$DIR/userhome/.gitconfig"

# Puts `gh` on the server's PATH (a group that starts without one leaves this out).
install_gh() {
  cp "$DIR/tools/gh" "$DIR/bin/gh"
  chmod +x "$DIR/bin/gh"
}

# checkout <folder under repos> [<origin url>]: a repository with one commit.
checkout() {
  local dir="$DIR/repos/$1"
  git init -q -b main "$dir"
  [ -n "$2" ] && git -C "$dir" remote add origin "$2"
  printf '# %s\n' "$1" > "$dir/README.md"
  git -C "$dir" add -A
  git -C "$dir" commit -qm init
}

# The fixture's project (`$DIR/workspace`) with this origin; the test commits into it afterwards.
workspace_origin() {
  git init -q -b main "$DIR/workspace"
  git -C "$DIR/workspace" remote add origin "$1"
}

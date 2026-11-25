# Sourced by each `*-gitlab*.sh` in the group's folder (`$DIR`): puts the stand-in
# `glab` and the projects it answers for in place, and makes the checkouts the
# group's projects are on. Nothing here reaches a network: remotes are only names.

GL_FIXTURES="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

cp -R "$GL_FIXTURES/data" "$DIR/glab-data"
: > "$DIR/glab-data/rules"
: > "$DIR/glab-data/log"
mkdir -p "$DIR/tools" "$DIR/repos"
cp "$GL_FIXTURES/glab-rule" "$DIR/bin/glab-rule"
cp "$GL_FIXTURES/glab" "$DIR/tools/glab"
chmod +x "$DIR/bin/glab-rule" "$DIR/tools/glab"

printf '[user]\n\tname = t\n\temail = t@t\n[init]\n\tdefaultBranch = main\n' > "$DIR/userhome/.gitconfig"

# Puts `glab` on the server's PATH (a group that starts without one leaves this out).
install_glab() {
  cp "$DIR/tools/glab" "$DIR/bin/glab"
  chmod +x "$DIR/bin/glab"
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

# Projects made, changed and deleted; the scratch project and its threads' folders;
# new named projects, whose first commit can fail in three ways.
write_gitconfig() {
cat > userhome/.gitconfig <<'CFG'
[user]
	name = t
	email = t@t
[init]
	defaultBranch = main
CFG
}
write_gitconfig

# git, failing on `init` while the file `git-init-fails` is there.
cat > bin/git <<'SH'
#!/bin/sh
here=$(cd "$(dirname "$0")/.." && pwd)
if [ -f "$here/git-init-fails" ] && [ "$1" = init ]; then
  echo "fatal: cannot init here" >&2
  exit 1
fi
exec /usr/bin/git "$@"
SH
chmod +x bin/git

# The provider's CLI: fails at once, or (while the file `hang` is there) keeps a turn going.
cat > bin/claude-stub <<'SH'
#!/bin/sh
here=$(cd "$(dirname "$0")/.." && pwd)
[ -f "$here/hang" ] && sleep 40
exit 1
SH
chmod +x bin/claude-stub

# A hook that refuses every commit.
mkdir -p hooks
cat > hooks/pre-commit <<'SH'
#!/bin/sh
echo "the hook says no" >&2
exit 1
SH
chmod +x hooks/pre-commit

# A checkout with a remote, so the project has a repository identity.
mkdir -p with-remote plain-folder
( cd with-remote && /usr/bin/git init -q -b main && /usr/bin/git remote add origin https://github.com/acme/widgets.git )
printf 'a file\n' > a-file

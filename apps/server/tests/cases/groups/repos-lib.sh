# Shared by the `repos-*.sh` setup scripts (sourced in $DIR before the server starts).
#
# Nothing here reaches the network:
# - `bin/gh` stands in for the GitHub CLI and answers from canned data, by its arguments;
# - `bin/ssh` stands in for ssh as git runs it: `git@github.test:<path>` is served from
#   `$DIR/remotes/<path>`, `denied.test` and `hostkey.test` refuse like a host without the
#   user's key, `slow.test` never answers, `<name>.says.test` prints the file `ssh-says/<name>`
#   and fails (or, for a name ending in `-hang`, never answers);
# - `https://github.test/`, `https://denied.test/` and `https://hostkey.test/` are rewritten
#   by the test's own `~/.gitconfig` to `file://$DIR/remotes/`.
#
# A project's clone runs after `projectClone.start` has answered. A case that reads what
# became of it waits for it in its `sh`, with the commands written at the end of this file
# (`await-done`, `await-ssh`, `await-exit`, `sleep-until`), instead of sleeping for a guess.

mkdir -p "$DIR/remotes/acme" "$DIR/bin" "$DIR/userhome" "$DIR/stars" "$DIR/clones" "$DIR/work" "$DIR/ssh-calls"

cat > "$DIR/userhome/.gitconfig" <<EOF
[user]
	name = t
	email = t@t
[init]
	defaultBranch = main
[core]
	sshCommand = $DIR/bin/ssh
[url "file://$DIR/remotes/"]
	insteadOf = https://github.test/
	insteadOf = https://denied.test/
	insteadOf = https://hostkey.test/
EOF

# The repository the clones are of: three commits, one delta.
git init -q -b main "$DIR/seed"
for i in 1 2 3; do
  echo "line $i" > "$DIR/seed/f$i.txt"
  git -C "$DIR/seed" add -A
  git -C "$DIR/seed" commit -qm "c$i"
done
git clone -q --bare "$DIR/seed" "$DIR/remotes/acme/local.git"

cat > "$DIR/bin/ssh" <<'EOF'
#!/bin/bash
# Stand-in for ssh as git runs it: ssh [-o option]... [-p port] user@host "git-upload-pack 'path'".
HERE=$(cd "$(dirname "$0")/.." && pwd)
while [ $# -gt 0 ]; do
  case "$1" in
    -o|-p) shift 2 ;;
    -*) shift ;;
    *) break ;;
  esac
done
host=$1
shift
command="$*"
# One line per run, for `await-ssh`; a host that prints is counted once it has.
called() {
  echo "$command" >> "$HERE/ssh-calls/${host#git@}"
}
case "$host" in
  git@github.test)
    called
    program=${command%% *}
    path=${command#* }
    path=${path#\'}
    path=${path%\'}
    path=${path#/}
    exec git "${program#git-}" "$HERE/remotes/$path"
    ;;
  git@denied.test)
    echo "git@denied.test: Permission denied (publickey)." >&2
    called
    exit 255
    ;;
  git@hostkey.test)
    echo "Host key verification failed." >&2
    called
    exit 255
    ;;
  git@echo.test)
    echo "fatal: unable to access 'https://user:secret@echo.test/acme/x.git/': The requested URL returned error: 403" >&2
    called
    exit 128
    ;;
  git@slow.test)
    # Says nothing until git goes away (its end of the pipe closes).
    called
    read -r -t 100 _
    exit 255
    ;;
  git@*.says.test)
    # Prints what `ssh-says/<name>` holds, as git's own progress would come, then fails;
    # a name ending in `-hang` waits for git to go away instead.
    name=${host#git@}
    name=${name%.says.test}
    cat "$HERE/ssh-says/$name" >&2
    called
    case "$name" in
      *-hang) read -r -t 100 _ ;;
    esac
    exit 128
    ;;
esac
echo "ssh: Could not resolve hostname ${host#*@}: nodename nor servname provided, or not known" >&2
exit 255
EOF
chmod +x "$DIR/bin/ssh"

cat > "$DIR/bin/gh" <<'EOF'
#!/bin/bash
# Stand-in for the GitHub CLI: the commands domovoi-server runs, answered from canned data.
HERE=$(cd "$(dirname "$0")/.." && pwd)
echo "$GH_HOST|$PWD|$*" >> "$HERE/gh.log"
fail() {
  echo "$1" >&2
  exit "${2:-1}"
}
logged_out() {
  printf 'To get started with GitHub CLI, please run:  gh auth login\nAlternatively, populate the GH_TOKEN environment variable with a GitHub API authentication token.\n' >&2
  exit 4
}
# `gh repo view --json nameWithOwner,url,sshUrl` prints its keys in this order.
info() {
  printf '{"nameWithOwner":"%s","sshUrl":"%s","url":"%s"}\n' "$1" "$2" "$3"
}
case "$1 $2" in
  "repo view")
    [ "$GH_HOST" = github.com ] || fail "unexpected GH_HOST $GH_HOST"
    [ "$4 $5" = "--json nameWithOwner,url,sshUrl" ] || fail "unexpected arguments: $*"
    case "$3" in
      acme/widgets) info acme/widgets git@github.com:acme/widgets.git https://github.com/acme/widgets ;;
      probe/cwd) info "probe/$(basename "$PWD")" git@github.com:probe/cwd.git https://github.com/probe/cwd ;;
      acme/missing) fail "GraphQL: Could not resolve to a Repository with the name 'acme/missing'. (repository)" ;;
      acme/loggedout) logged_out ;;
      acme/silent) exit 1 ;;
      acme/garbled) echo "<html>502 Bad Gateway</html>" ;;
      acme/partial) echo '{"nameWithOwner":"acme/partial"}' ;;
      acme/odd-types) echo '{"nameWithOwner":7,"sshUrl":["git@github.com:acme/odd-types.git"],"url":null}' ;;
      acme/array) echo '[]' ;;
      # Clones and pushes: `github.test` is served by the stand-in ssh and the rewritten https URL.
      acme/local) info acme/local git@github.test:acme/local.git https://github.test/acme/local ;;
      acme/denied) info acme/denied git@denied.test:acme/local.git https://denied.test/acme/local ;;
      acme/hostkey) info acme/hostkey git@hostkey.test:acme/gone.git https://hostkey.test/acme/gone ;;
      acme/pub | acme/pub-public | acme/pub-two) info "$3" "git@github.test:$3.git" "https://github.test/$3" ;;
      acme/pub-denied) info "$3" git@denied.test:acme/pub-denied.git https://denied.test/acme/pub-denied ;;
      acme/pub-hostkey) info "$3" git@hostkey.test:acme/gone.git https://hostkey.test/acme/gone ;;
      acme/pub-nowhere) info "$3" git@github.test:acme/nowhere.git https://github.test/acme/nowhere ;;
      acme/vanished) fail "GraphQL: Could not resolve to a Repository with the name 'acme/vanished'. (repository)" ;;
      *) fail "GraphQL: Could not resolve to a Repository with the name '$3'. (repository)" ;;
    esac
    ;;
  "repo create")
    [ "$GH_HOST" = github.com ] || fail "unexpected GH_HOST $GH_HOST"
    [ $# = 4 ] || fail "unexpected arguments: $*"
    # A name ending in `-public` is created public, every other one private.
    case "$3" in
      *-public) [ "$4" = --public ] || fail "expected --public for $3, got $4" ;;
      *) [ "$4" = --private ] || fail "expected --private for $3, got $4" ;;
    esac
    case "$3" in
      acme/taken) fail "GraphQL: Name already exists on this account (createRepository)" ;;
      acme/loggedout) logged_out ;;
    esac
    echo "https://github.com/$3"
    ;;
  "api -X")
    # gh api -X PUT user/starred/<owner>/<name> --silent
    [ "$3 $5" = "PUT --silent" ] || fail "unexpected arguments: $*"
    case "$4" in
      user/starred/acme/forbidden) fail "gh: Resource not accessible by integration (HTTP 403)" ;;
      user/starred/*) mkdir -p "$HERE/stars/$GH_HOST/$(dirname "${4#user/starred/}")" && touch "$HERE/stars/$GH_HOST/${4#user/starred/}" ;;
      *) fail "gh: Not Found (HTTP 404)" ;;
    esac
    ;;
  api\ user/starred/*)
    # gh api user/starred/<owner>/<name> --silent
    [ "$3" = --silent ] || fail "unexpected arguments: $*"
    [ -e "$HERE/stars/$GH_HOST/${2#user/starred/}" ] || fail "gh: Not Found (HTTP 404)"
    ;;
  *) fail "unknown command: $*" ;;
esac
EOF
chmod +x "$DIR/bin/gh"

# await-done <project id> [n]: returns once the server has recorded the repository of the
# project's clone for the n-th time (default: the first). That is the last thing it does for a
# clone that is done, so the clone's snapshot is final by then. Leaves `done-at-<project id>`.
cat > "$DIR/bin/await-done" <<'EOF'
#!/bin/bash
HERE=$(cd "$(dirname "$0")/.." && pwd)
command -v sqlite3 > /dev/null || { echo "await-done needs the sqlite3 command" >&2; exit 1; }
for _ in $(seq 1 600); do
  db=$(find "$HERE/home" -name domovoi.sqlite 2> /dev/null | head -1)
  if [ -n "$db" ]; then
    have=$(sqlite3 -readonly "$db" "select count(*) from events where aggregate_id = '$1' and type = 'project.updated'" 2> /dev/null)
    if [ "${have:-0}" -ge "${2:-1}" ]; then
      touch "$HERE/done-at-$1"
      exit 0
    fi
  fi
  sleep 0.1
done
echo "the clone of project $1 was not done within a minute" >&2
exit 1
EOF

# await-ssh <host> [n]: returns once the stand-in ssh has been run for <host> n times
# (default: once) and has printed what it prints for that host.
cat > "$DIR/bin/await-ssh" <<'EOF'
#!/bin/bash
HERE=$(cd "$(dirname "$0")/.." && pwd)
for _ in $(seq 1 600); do
  have=$(wc -l < "$HERE/ssh-calls/$1" 2> /dev/null)
  [ "${have:-0}" -ge "${2:-1}" ] && exit 0
  sleep 0.1
done
echo "ssh was not run for $1 ${2:-1} time(s) within a minute" >&2
exit 1
EOF

# await-exit <folder>: returns once no `git clone` into a folder of that name runs. For a clone
# known to have started (`await-ssh` first); the server records its end a moment later.
cat > "$DIR/bin/await-exit" <<'EOF'
#!/bin/bash
for _ in $(seq 1 600); do
  pgrep -f "clone --progress -- .* $1\$" > /dev/null || exit 0
  sleep 0.1
done
echo "git clone into $1 still runs after a minute" >&2
exit 1
EOF

# sleep-until <file> <seconds>: returns <seconds> (give or take one) after <file> was last written.
cat > "$DIR/bin/sleep-until" <<'EOF'
#!/bin/bash
HERE=$(cd "$(dirname "$0")/.." && pwd)
at=$(stat -f %m "$HERE/$1" 2> /dev/null || stat -c %Y "$HERE/$1")
left=$((at + $2 - $(date +%s)))
[ "$left" -gt 0 ] && sleep "$left"
exit 0
EOF
chmod +x "$DIR/bin/await-done" "$DIR/bin/await-ssh" "$DIR/bin/await-exit" "$DIR/bin/sleep-until"

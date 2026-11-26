# Shared by the vcs-*.sh setup scripts (each sources it): an identity and fixed
# dates for commits, and stand-ins in $DIR/bin for ssh, gh and the text
# generation CLIs. A remote `git@github.com:owner/name.git` is served from
# $DIR/remotes/owner/name.git through the ssh stand-in, so a checkout can have
# a GitHub remote that fetches and pushes without a network.

export GIT_AUTHOR_DATE="2024-01-02T03:04:05Z" GIT_COMMITTER_DATE="2024-01-02T03:04:05Z"

cat > "$DIR/userhome/.gitconfig" <<CONFIG
[user]
	name = Test Author
	email = author@example.test
[init]
	defaultBranch = main
[core]
	sshCommand = $DIR/bin/fakessh
[ssh]
	variant = simple
[advice]
	detachedHead = false
CONFIG

# ssh: `fakessh git@host "git-upload-pack 'owner/name.git'"`.
cat > "$DIR/bin/fakessh" <<'SH'
#!/bin/sh
D="$(cd "$(dirname "$0")/.." && pwd)"
for last; do :; done
cmd="${last%% *}"
path="${last#* }"
path="${path#\'}"
path="${path%\'}"
exec "$cmd" "$D/remotes/$path"
SH

# git with the fixed dates, for the `sh` of a case.
cat > "$DIR/bin/gitc" <<'SH'
#!/bin/sh
GIT_AUTHOR_DATE="2024-01-02T03:04:05Z" GIT_COMMITTER_DATE="2024-01-02T03:04:05Z" exec git "$@"
SH
chmod +x "$DIR/bin/fakessh" "$DIR/bin/gitc"

mkdir -p "$DIR/remotes" "$DIR/repos"

# new_repo <name>: a repository in $DIR/repos/<name> with one commit on main.
new_repo() {
  mkdir -p "$DIR/repos/$1"
  git -C "$DIR/repos/$1" init -q -b main
  printf 'def add(a, b):\n    return a + b\n' > "$DIR/repos/$1/calc.py"
  printf '# Calc\n\nAdds numbers.\n' > "$DIR/repos/$1/README.md"
  git -C "$DIR/repos/$1" add -A
  git -C "$DIR/repos/$1" commit -qm "Add calc"
}

# new_remote <owner/name>: a bare repository behind git@github.com:<owner/name>.git, with one commit on main.
new_remote() {
  new_repo "seed-$$"
  mkdir -p "$DIR/remotes/$(dirname "$1")"
  git clone -q --bare "$DIR/repos/seed-$$" "$DIR/remotes/$1.git"
  rm -rf "$DIR/repos/seed-$$"
}

# clone_remote <owner/name> <name> [host]: a checkout of it in $DIR/repos/<name>.
clone_remote() {
  git clone -q "git@${3:-github.com}:$1.git" "$DIR/repos/$2"
}

# install_gh: a stand-in for `gh` in $DIR/bin, answering from $DIR/gh:
#   gh/fail                   every command fails with this text on stderr
#   gh/view/<reference>.json  `gh pr view <reference>` (characters other than letters and digits are `_`)
#   gh/list/<branch>.json     `gh pr list --head <branch>`; `[]` without one
#   gh/checkout-fail, gh/create-fail   that command fails with this text
#   gh/create-silent          `gh pr create` succeeds but the pull request is not listed afterwards
# `gh pr create` writes gh/list/<branch>.json from its arguments, so the next
# `gh pr list` finds the pull request; `gh pr checkout <n>` fetches
# refs/pull/<n>/head into the branch `pr-<n>` and checks it out.
install_gh() {
  mkdir -p "$DIR/gh/view" "$DIR/gh/list"
  cat > "$DIR/bin/gh" <<'SH'
#!/bin/sh
D="$(cd "$(dirname "$0")/.." && pwd)"
key() { printf '%s' "$1" | tr -c 'A-Za-z0-9' '_'; }
fail() { cat "$1" >&2; exit 1; }
[ "$1" = "--version" ] && { echo "gh version 2.99.0 (2026-01-01)"; exit 0; }
echo "$@" >> "$D/gh/log"
[ -f "$D/gh/fail" ] && fail "$D/gh/fail"
case "$1 $2" in
  "pr view")
    f="$D/gh/view/$(key "$3").json"
    [ -f "$f" ] || { echo "GraphQL: Could not resolve to a PullRequest with the number of $3. (repository.pullRequest)" >&2; exit 1; }
    cat "$f" ;;
  "pr checkout")
    [ -f "$D/gh/checkout-fail" ] && fail "$D/gh/checkout-fail"
    git fetch -q origin "refs/pull/$3/head" && git checkout -q -B "pr-$3" FETCH_HEAD ;;
  "pr list")
    shift 2; head=""
    while [ $# -gt 0 ]; do [ "$1" = "--head" ] && head="$2"; shift; done
    cat "$D/gh/list/$(key "$head").json" 2>/dev/null || echo "[]" ;;
  "pr create")
    [ -f "$D/gh/create-fail" ] && fail "$D/gh/create-fail"
    shift 2; head=""; title="(filled from the commits)"; base="(no base given)"
    while [ $# -gt 0 ]; do
      case "$1" in
        --head) head="$2" ;;
        --title) title="$2" ;;
        --base) base="$2" ;;
      esac
      shift
    done
    [ -f "$D/gh/create-silent" ] || printf '[{"number":12,"url":"https://github.com/acme/widgets/pull/12","title":"%s","baseRefName":"%s","headRefName":"%s"}]\n' "$title" "$base" "$head" > "$D/gh/list/$(key "$head").json"
    echo "https://github.com/acme/widgets/pull/12" ;;
  *) echo "unknown command: $*" >&2; exit 2 ;;
esac
SH
  chmod +x "$DIR/bin/gh"
}

# install_textgen: $DIR/bin/textgen stands in for both text generation CLIs
# (`codex exec …` and `claude -p …`). It answers the prompt it is given (a
# commit message, with or without a branch, or a pull request text), naming in
# the answer the CLI, model and effort it was started with and whether the
# prompt carried the user's own instructions or a template. $DIR/textgen/mode
# picks what it does instead: see the cases below.
install_textgen() {
  mkdir -p "$DIR/textgen"
  cat > "$DIR/bin/textgen" <<'SH'
#!/bin/sh
D="$(cd "$(dirname "$0")/.." && pwd)"
mode="$(cat "$D/textgen/mode" 2>/dev/null || echo ok)"
prompt="$(cat)"
cli=claude; [ "$1" = "exec" ] && cli=codex
out=""; model=""; effort=""
while [ $# -gt 0 ]; do
  case "$1" in
    --output-last-message) out="$2" ;;
    --model) model="$2" ;;
    --effort) effort="$2" ;;
    --config) effort="$(printf '%s' "${2#model_reasoning_effort=}" | tr -d '"')" ;;
  esac
  shift
done
by="$cli $model ${effort:-no-effort}"
case "$prompt" in *"Additional instructions:"*) by="$by, instructed" ;; esac
case "$prompt" in *"Repository change request template:"*) by="$by, templated" ;; esac
case "$prompt" in
  *"keys: subject, body, branch."*) answer="{\"subject\":\"Add generated change ($by).\",\"body\":\"- written by the stand-in\",\"branch\":\"Feat/Generated Name!\"}" ;;
  *"keys: subject, body."*) answer="{\"subject\":\"  Generated subject ($by).\\nA second line\",\"body\":\"  A body.  \"}" ;;
  *"keys: title, body."*) answer="{\"title\":\" Generated title ($by) \",\"body\":\"## Summary\\n- generated\\n\"}" ;;
  *) answer="{}" ;;
esac
case "$mode" in
  empty-branch) answer="{\"subject\":\"Subject becomes the branch\",\"body\":\"\",\"branch\":\"  \"}" ;;
  no-subject) answer="{\"subject\":\".\",\"body\":\"x\"}" ;;
  no-keys) answer="{\"other\":true}" ;;
  no-title) answer="{\"title\":\"  \",\"body\":\"x\"}" ;;
esac
escaped="$(printf '%s' "$answer" | sed 's/\\/\\\\/g; s/"/\\"/g')"
if [ "$cli" = codex ]; then
  case "$mode" in
    silent) exit 0 ;;
    not-json) echo "sorry" > "$out" ;;
    fail) for n in 1 2 3 4 5 6 7; do echo "codex: problem line $n" >&2; done; exit 2 ;;
    fail-quiet) exit 3 ;;
    fenced) printf '```json\n%s\n```\n' "$answer" > "$out" ;;
    *) printf '%s\n' "$answer" > "$out" ;;
  esac
else
  case "$mode" in
    list) printf '[{"type":"system"},{"type":"result","structured_output":%s}]\n' "$answer" ;;
    fenced) printf '{"type":"result","result":"```json\\n%s\\n```"}\n' "$escaped" ;;
    error-result) echo '{"type":"result","is_error":true,"result":"Credit balance is too low"}' ;;
    error-list) echo '{"type":"result","is_error":true,"errors":["[ede_diagnostic] noise","First problem","Second problem"]}' ;;
    error-empty) echo '{"type":"result","is_error":true,"result":"  "}' ;;
    not-json) echo "Not logged in" ;;
    fail) echo "claude: it broke" >&2; exit 3 ;;
    fail-quiet) exit 3 ;;
    no-structured) echo '{"type":"result","result":"I would rather answer in prose."}' ;;
    *) printf '{"type":"result","is_error":false,"structured_output":%s}\n' "$answer" ;;
  esac
fi
SH
  chmod +x "$DIR/bin/textgen"
}

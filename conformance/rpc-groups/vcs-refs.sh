source "$(dirname "${BASH_SOURCE[0]}")/vcs-common.sh"
R="$DIR/repos"

mkdir -p "$R/notrepo" "$R/tobe" "$R/tobe-git" "$R/readonly"
chmod 555 "$R/readonly"
echo "a file" > "$DIR/afile"
git init -q -b main "$R/empty"

# Local branches, one of them in a linked worktree, and a tag.
new_repo local
( cd "$R/local"
  git branch feature/one && git branch feature/two && git branch fix-typo
  git tag v1
  mkdir sub && echo s > sub/s.txt && git add -A && git commit -qm "Sub"
  git worktree add -q "$R/local-fix" fix-typo )

# A GitHub remote with branches of its own.
new_remote acme/widgets
clone_remote acme/widgets seed
( cd "$R/seed"
  for b in feature/remote-only shared renamed-upstream taken; do
    git checkout -q -b "$b" main && echo "$b" > branch.txt
    git add -A && git commit -qm "On $b" && git push -q -u origin "$b"
  done )
clone_remote acme/widgets work
( cd "$R/work"
  git checkout -q -b mine origin/renamed-upstream   # tracks origin/renamed-upstream under another name
  git checkout -q main
  git branch --no-track taken main                  # same name as origin/taken, tracking nothing
  git checkout -q shared && git checkout -q main )  # a local branch that tracks origin/shared

# Pull: up to date, behind, diverged, without an upstream, detached.
for n in uptodate behind diverged; do clone_remote acme/widgets "pull-$n"; done
( cd "$R/seed" && git checkout -q main && echo "new" > new.txt && git add -A && git commit -qm "New on main" && git push -q )
( cd "$R/pull-uptodate" && git pull -q )
( cd "$R/pull-diverged" && echo "mine" > mine.txt && git add -A && git commit -qm "Mine" )
new_repo pull-noupstream
new_repo pull-detached
git -C "$R/pull-detached" checkout -q --detach

# A dirty tree that a checkout of `other` would overwrite.
new_repo blocked
( cd "$R/blocked" && git checkout -q -b other && echo "other" > README.md && git commit -qam "Other" && git checkout -q main && echo "dirty" > README.md )

# Worktrees.
new_repo trees
git -C "$R/trees" branch spare
mkdir -p "$R/occupied" && echo "here" > "$R/occupied/file"

# More than 200 commits ahead of main.
new_repo many
( cd "$R/many"
  git checkout -q -b long
  { for i in $(seq 1 203); do
      echo "commit refs/heads/long"
      echo "committer Test Author <author@example.test> $((1704164645 + i)) +0000"
      echo "data <<MSG"; echo "Step $i"; echo "MSG"
      [ "$i" = 1 ] && echo "from refs/heads/long^0"
      echo
    done; } | git fast-import --quiet
  git checkout -q -b short main && echo s > s.txt && git add -A && git commit -qm "Short one" && echo t > t.txt && git add -A && git commit -qm "Short two: with | and \"quotes\"" )

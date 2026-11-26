source "$(dirname "${BASH_SOURCE[0]}")/vcs-common.sh"
R="$DIR/repos"

mkdir -p "$R/notrepo"
echo "plain" > "$R/notrepo/file.txt"

# No commit yet: nothing at all, and with files staged and untracked.
git init -q -b main "$R/empty"
git init -q -b main "$R/unborn"
echo "one" > "$R/unborn/staged.txt"
printf 'a\nb\n' > "$R/unborn/untracked.txt"
git -C "$R/unborn" add staged.txt

new_repo clean

# Every kind of change at once.
new_repo dirty
( cd "$R/dirty"
  mkdir sub
  printf 'one\ntwo\nthree\n' > sub/keep.txt
  echo "gone" > removed.txt
  echo "moved" > old-name.txt
  printf '\000\001\002' > data.bin
  echo "*.log" > .gitignore
  git add -A && git commit -qm "More files"
  printf 'def add(a, b):\n    return a + b\n\ndef sub(a, b):\n    return a - b\n' > calc.py
  echo "staged" > added.txt && git add added.txt
  git rm -q removed.txt
  git mv old-name.txt new-name.txt
  printf '\000\003\004\005' > data.bin
  printf 'x\ny\nz\n' > untracked.txt
  printf '\000\377' > untracked.bin
  echo "deep" > sub/new.txt
  echo "ignored" > debug.log )

new_repo staged
( cd "$R/staged" && echo "more" >> README.md && git add README.md )

new_repo detached
( cd "$R/detached" && echo 2 > two.txt && git add -A && git commit -qm "Second" && git checkout -q --detach HEAD~1 )

# A GitHub remote; main is two ahead of and one behind origin/main.
new_remote acme/widgets
clone_remote acme/widgets tracking
clone_remote acme/widgets other
( cd "$R/other" && echo "theirs" > theirs.txt && git add -A && git commit -qm "Theirs" && git push -q )
( cd "$R/tracking" && echo 1 > one.txt && git add -A && git commit -qm "One" && echo 2 > two.txt && git add -A && git commit -qm "Two" && git fetch -q )

# A branch that is not the default one: without and with an upstream.
clone_remote acme/widgets feature
( cd "$R/feature" && git checkout -q -b feature/x && echo f > f.txt && git add -A && git commit -qm "Feature" && echo g >> f.txt )
clone_remote acme/widgets pushed
( cd "$R/pushed" && git checkout -q -b feature/pushed && echo f > f.txt && git add -A && git commit -qm "Feature" && git push -q -u origin feature/pushed )

# Remotes of other hosts (never fetched), a remote not named origin, a remote without a default branch.
for spec in gitlab=git@gitlab.com:group/project.git bitbucket=https://bitbucket.org/team/repo.git selfhosted=ssh://git@git.example.test:2222/team/repo.git local=/somewhere/else; do
  new_repo "${spec%%=*}"
  git -C "$R/${spec%%=*}" remote add origin "${spec#*=}"
done
new_repo fork
git -C "$R/fork" remote add zeta https://github.com/zeta/calc.git
git -C "$R/fork" remote add alpha https://github.com/alpha/calc
new_repo master
git -C "$R/master" branch -m main master
new_repo trunk
git -C "$R/trunk" branch -m main trunk

# A merge stopped at a conflict.
new_repo conflict
( cd "$R/conflict"
  git checkout -q -b theirs && echo "theirs" > README.md && git commit -qam "Theirs"
  git checkout -q main && echo "ours" > README.md && git commit -qam "Ours"
  git merge -q theirs >/dev/null 2>&1 || true )

# A linked worktree.
new_repo trees
git -C "$R/trees" worktree add -q -b side "$R/trees-side"
echo "side" > "$R/trees-side/side.txt"

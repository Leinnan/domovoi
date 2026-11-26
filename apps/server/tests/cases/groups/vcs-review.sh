source "$(dirname "${BASH_SOURCE[0]}")/vcs-common.sh"
R="$DIR/repos"

mkdir -p "$R/notrepo"
new_repo clean

# No commit yet, with files staged and untracked.
git init -q -b main "$R/unborn"
echo "one" > "$R/unborn/staged.txt"
printf 'a\nb\n' > "$R/unborn/untracked.txt"
git -C "$R/unborn" add staged.txt

# A branch two commits ahead of main, with every kind of uncommitted change on top.
new_repo work
( cd "$R/work"
  printf 'one\ntwo\nthree\n' > keep.txt
  echo "gone" > removed.txt
  printf 'first line\nsecond line\nthird line\nfourth line\n' > old-name.txt
  printf '\000\001\002' > data.bin
  printf '\357\273\277with a mark\n' > bom.txt
  mkdir dir && echo "inside" > dir/inside.txt
  echo "*.log" > .gitignore
  git add -A && git commit -qm "More files"
  git checkout -q -b feature/review
  printf 'def add(a, b):\n    return a + b\n\ndef sub(a, b):\n    return a - b\n' > calc.py
  git commit -qam "Add sub"
  git mv old-name.txt new-name.txt && echo "fifth line" >> new-name.txt
  echo "committed on the branch" > branch.txt
  git add -A && git commit -qm "Rename, and a file for the branch." -m "With a body."
  printf 'def add(a, b):\n    return a + b\n\ndef sub(a,   b):\n    return a - b\n\ndef mul(a, b):\n    return a * b\n' > calc.py
  echo "staged" > added.txt && git add added.txt
  git rm -q removed.txt
  printf '\000\003\004\005' > data.bin
  printf 'x\ny\nz\n' > untracked.txt
  printf '\000\377' > untracked.bin
  echo "ignored" > debug.log
  printf '\357\273\277with a mark, changed\n' > bom.txt
  ln -s ../outside-target outside-link )
echo "outside the checkout" > "$R/outside-target"

# A detached head, and a branch with no base to compare with.
new_repo detached
( cd "$R/detached" && echo 2 > two.txt && git add -A && git commit -qm "Second" && git checkout -q --detach && echo 3 > two.txt )
new_repo trunk
( cd "$R/trunk" && git branch -m main trunk && echo t >> README.md )

# Files at the limits: a diff over 120 000 bytes, a file over 1 MB, a binary one, one that cannot be read.
new_repo big
( cd "$R/big"
  echo "small" > grows.txt
  echo "small" > huge.txt
  echo "text" > becomes-binary.txt
  awk 'BEGIN { for (i = 0; i < 30000; i++) print "committed line " i " padded out to be a longer line of text" }' > committed-huge.txt
  printf '\000\001' > committed.bin
  git add -A && git commit -qm "Files"
  awk 'BEGIN { for (i = 0; i < 4000; i++) print "line " i " of a long file, long enough to pass the limit" }' > grows.txt
  awk 'BEGIN { for (i = 0; i < 30000; i++) print "line " i " padded out to be a longer line of text" }' > huge.txt
  printf 'bin\000ary' > becomes-binary.txt
  echo "small again" > committed-huge.txt
  echo "text now" > committed.bin )
new_repo locked
( cd "$R/locked" && echo "secret" > unreadable.txt && chmod 000 unreadable.txt )

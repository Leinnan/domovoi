source "$(dirname "${BASH_SOURCE[0]}")/vcs-common.sh"
install_gh
R="$DIR/repos"

# Text generation CLIs that cannot be used: one that does not start, one that is not there.
printf '#!/no/such/interpreter\n' > "$DIR/bin/broken-codex"
chmod +x "$DIR/bin/broken-codex"

mkdir -p "$R/notrepo"
new_repo noremote
echo "change" >> "$R/noremote/README.md"
new_repo gitlab
git -C "$R/gitlab" remote add origin git@gitlab.com:group/project.git
new_repo elsewhere
git -C "$R/elsewhere" remote add origin /somewhere/else

# The GitHub repository, with the heads of pull requests 7, 8 and 14 as GitHub keeps them.
new_remote acme/widgets
clone_remote acme/widgets seed
( cd "$R/seed"
  git checkout -q -b feature/seven && echo 7 > seven.txt && git add -A && git commit -qm "Seven" && git push -q origin HEAD:refs/pull/7/head
  git checkout -q -b fix-thing main && echo 8 > eight.txt && git add -A && git commit -qm "Eight" && git push -q origin HEAD:refs/pull/8/head HEAD:refs/pull/14/head )
SEVEN="$(git -C "$R/seed" rev-parse feature/seven)"

for name in local trees dirty; do clone_remote acme/widgets "$name"; done
echo "uncommitted" >> "$R/dirty/README.md"

pr() { # pr <file> <number> <title> <head branch> <state> <merged at> <head sha> <cross repository> <owner json> <repository json>
  printf '{"number":%s,"title":"%s","url":"https://github.com/acme/widgets/pull/%s","baseRefName":"main","headRefName":"%s","state":"%s","mergedAt":%s,"headRefOid":%s,"isCrossRepository":%s,"headRepositoryOwner":%s,"headRepository":%s}\n' \
    "$2" "$3" "$2" "$4" "$5" "$6" "$7" "$8" "$9" "${10}" > "$DIR/gh/view/$1.json"
}
pr 7 7 "Seven" feature/seven OPEN null "\"$SEVEN\"" false '{"login":"acme"}' '{"name":"widgets"}'
cp "$DIR/gh/view/7.json" "$DIR/gh/view/https___github_com_acme_widgets_pull_7.json"
pr 8 8 "Eight, from a fork" Fix/Thing OPEN null '"0000000000000000000000000000000000000000"' true '{"login":"forker"}' '{"name":"widgets"}'
pr 9 9 "Nine, closed" feature/nine CLOSED null null false null null
pr 10 10 "Ten, merged" feature/ten MERGED '"2024-02-03T04:05:06Z"' null false null null
pr 11 11 "Eleven, merged by its date" feature/eleven OPEN '"2024-02-03T04:05:06Z"' null false null null
pr 13 13 "Thirteen, whose head is gone" feature/thirteen OPEN null null false null null
pr 14 14 "Fourteen, from a fork" feature/fourteen OPEN null "\"$SEVEN\"" true '{"login":"forker"}' '{"name":"widgets"}'
pr 15 15 "Fifteen, from main" main OPEN null null false '{"login":"acme"}' '{"name":"widgets"}'
pr 16 16 "Sixteen, whose head is gone" feature/sixteen OPEN null null true '{"login":"forker"}' '{"name":"widgets"}'
echo '{"number":17}' > "$DIR/gh/view/17.json"
echo 'not json' > "$DIR/gh/view/18.json"

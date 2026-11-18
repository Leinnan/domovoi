source "$(dirname "${BASH_SOURCE[0]}")/vcs-common.sh"
install_gh
install_textgen
R="$DIR/repos"

mkdir -p "$R/notrepo"
new_repo plain
new_repo detached
git -C "$R/detached" checkout -q --detach
new_repo hooked
printf '#!/bin/sh\necho "the hook says no" >&2\necho "and says why" >&2\nexit 1\n' > "$R/hooked/.git/hooks/pre-commit"
chmod +x "$R/hooked/.git/hooks/pre-commit"
git init -q -b main "$R/unborn"
echo "first" > "$R/unborn/first.txt"

new_remote acme/widgets
new_remote group/project

# On the default branch of a GitHub checkout.
clone_remote acme/widgets onmain

# Feature branches of GitHub checkouts: <name>=<pushed|unpushed>.
for spec in topic=unpushed pushed=pushed ahead=pushed existing=pushed failing=pushed silent=pushed filled=pushed untitled=pushed templated=pushed plainly=pushed nobase=pushed loggedout=pushed stacked=unpushed; do
  name="${spec%%=*}"
  clone_remote acme/widgets "$name"
  ( cd "$R/$name"
    git checkout -q -b "feature/$name" && echo "$name" > "$name.txt" && git add -A && git commit -qm "Work on $name"
    [ "${spec#*=}" = pushed ] && git push -q -u origin "feature/$name" || true )
done
( cd "$R/ahead" && echo more >> ahead.txt && git commit -qam "More on ahead" )
git -C "$R/nobase" remote set-head origin -d
mkdir -p "$R/templated/.github" "$R/plainly/.github"
printf '## What\n\n<!-- say what -->\n' > "$R/templated/.github/pull_request_template.md"
cp "$R/templated/.github/pull_request_template.md" "$R/plainly/.github/"
printf '[{"number":3,"url":"https://github.com/acme/widgets/pull/3","title":"An older pull request with a title that is long enough to be cut off in the toast","baseRefName":"main","headRefName":"feature/existing"}]\n' > "$DIR/gh/list/feature_existing.json"

# The remote moved on: a push is refused.
clone_remote acme/widgets refused
clone_remote acme/widgets mover
( cd "$R/mover" && git checkout -q -b feature/refused && echo 1 > r.txt && git add -A && git commit -qm "Start" && git push -q -u origin feature/refused )
( cd "$R/refused" && git fetch -q && git checkout -q feature/refused && echo mine > mine.txt && git add -A && git commit -qm "Mine" )
( cd "$R/mover" && echo 2 >> r.txt && git commit -qam "Theirs" && git push -q )

# A checkout whose remote is not GitHub.
clone_remote group/project gitlab gitlab.com
( cd "$R/gitlab" && git checkout -q -b feature/lab && echo lab > lab.txt && git add -A && git commit -qm "Lab" )

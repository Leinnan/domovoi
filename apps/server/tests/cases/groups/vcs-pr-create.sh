# Prepares the folder of the `vcs-pr-create` RPC group: GitHub and GitLab checkouts whose remotes are served from
# $DIR/remotes, with stand-ins for `gh`, `glab` and the text generation CLIs. The cases make threads on them, link
# the issue each was started on, and make pull requests through the sheet's two calls.
source "$(dirname "${BASH_SOURCE[0]}")/gitlab-fixtures/setup.sh"
source "$(dirname "${BASH_SOURCE[0]}")/vcs-common.sh"
install_gh
install_glab
install_textgen
R="$DIR/repos"

mkdir -p "$R/notrepo"
new_repo plain
new_remote acme/widgets
new_remote group/project

# The project's own folder: on the default branch, nothing changed yet.
clone_remote acme/widgets work

# A feature branch with a commit that was never pushed, a changed file and a new one.
clone_remote acme/widgets topic
( cd "$R/topic"
  git checkout -q -b feature/topic && echo topic > topic.txt && git add -A && git commit -qm "Work on topic"
  echo more >> topic.txt && echo new > topic2.txt )

# A pushed feature branch, for when `gh` is signed out.
clone_remote acme/widgets loggedout
( cd "$R/loggedout" && git checkout -q -b feature/loggedout && echo x > x.txt && git add -A && git commit -qm "Work" && git push -q -u origin feature/loggedout )

# A GitLab checkout with a commit to open a merge request for.
clone_remote group/project lab gitlab.com
( cd "$R/lab" && git checkout -q -b feature/lab && echo lab > lab.txt && git add -A && git commit -qm "Lab work" )

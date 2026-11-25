# Prepares the folder of the `prs-gitlab-failures` RPC group: GitLab projects with no `glab` on the PATH yet
# (a case installs it), and the rules that make the stand-in misbehave.
source "$(dirname "${BASH_SOURCE[0]}")/gitlab-fixtures/setup.sh"
checkout gl-one https://gitlab.com/group/one.git
checkout gl-self git@gitlab.example.com:team/sub/app.git

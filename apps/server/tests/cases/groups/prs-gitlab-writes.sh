# Prepares the folder of the `prs-gitlab-writes` RPC group: a signed-in `glab`, a gitlab.com project, a self-managed
# GitLab one (recognized by its host's name) and a GitHub project whose `gh` is missing.
source "$(dirname "${BASH_SOURCE[0]}")/gitlab-fixtures/setup.sh"
install_glab
checkout gl-one https://gitlab.com/group/one.git
checkout gl-self git@gitlab.example.com:team/sub/app.git
checkout gh-one https://github.com/acme/calc.git

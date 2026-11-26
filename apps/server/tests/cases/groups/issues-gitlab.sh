# Prepares the folder of the `issues-gitlab` RPC group: a signed-in `glab`, a gitlab.com project and a self-managed
# GitLab one (recognized by its host's name).
source "$(dirname "${BASH_SOURCE[0]}")/gitlab-fixtures/setup.sh"
install_glab
checkout gl-one https://gitlab.com/group/one.git
checkout gl-self git@gitlab.example.com:team/sub/app.git

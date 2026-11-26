# Prepares the folder of the `issues-reads` RPC group: a signed-in `gh`, GitHub projects (one a second checkout of the
# fixture's repository, one of a repository `gh` cannot resolve), a GitLab project and one with no remote.
source "$(dirname "${BASH_SOURCE[0]}")/issues-fixtures/setup.sh"
install_gh
workspace_origin https://github.com/acme/calc.git
checkout calc-again git@github.com:Acme/Calc.git
checkout widgets https://github.com/acme/widgets.git
checkout locked https://github.com/acme/locked.git
checkout gl-one https://gitlab.com/group/one.git
checkout plain

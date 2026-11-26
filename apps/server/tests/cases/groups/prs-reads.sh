# Prepares the folder of the `prs-reads` RPC group: a signed-in `gh`, two GitHub projects and projects on other hosts.
source "$(dirname "${BASH_SOURCE[0]}")/prs-fixtures/setup.sh"
install_gh
workspace_origin https://github.com/acme/calc.git
checkout widgets git@github.com:acme/widgets.git
checkout widgets-again https://github.com/Acme/Widgets
checkout gl-one https://gitlab.com/group/one.git
checkout gl-two git@gitlab.com:group/sub/two.git
checkout bb https://bitbucket.org/team/app.git
checkout elsewhere https://git.example.com/team/app.git
checkout plain
checkout broken https://github.com/acme/broken.git

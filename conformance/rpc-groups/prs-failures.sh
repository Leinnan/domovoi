# Prepares the folder of the `prs-failures` RPC group: a signed-in `gh`, a GitHub project, a GitLab one and one with no remote.
source "$(dirname "${BASH_SOURCE[0]}")/prs-fixtures/setup.sh"
install_gh
workspace_origin https://github.com/acme/calc.git
checkout gl-one https://gitlab.com/group/one.git
checkout plain
printf 'not a program\n' > "$DIR/tools/not-executable"

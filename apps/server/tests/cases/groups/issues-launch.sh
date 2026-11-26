# Prepares the folder of the `issues-launch` RPC group: a signed-in `gh`, the fixture's GitHub project and a project
# with no remote. The cases start threads on issues, and make `gh` fail to show what a launch does then.
source "$(dirname "${BASH_SOURCE[0]}")/issues-fixtures/setup.sh"
install_gh
workspace_origin https://github.com/acme/calc.git
checkout plain

# Prepares the folder of the `issues-writes` RPC group: a signed-in `gh`, the fixture's GitHub project and one on a
# repository whose labels and assignees GitHub drops (no triage access).
source "$(dirname "${BASH_SOURCE[0]}")/issues-fixtures/setup.sh"
install_gh
workspace_origin https://github.com/acme/calc.git
checkout locked https://github.com/acme/locked.git

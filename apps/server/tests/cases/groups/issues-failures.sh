# Prepares the folder of the `issues-failures` RPC group: the fixture's GitHub project with no `gh` on the PATH at
# first (the cases put it there and make it misbehave).
source "$(dirname "${BASH_SOURCE[0]}")/issues-fixtures/setup.sh"
workspace_origin https://github.com/acme/calc.git

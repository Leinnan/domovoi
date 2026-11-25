# Prepares the folder of the `issues-mcp` RPC group: a signed-in `gh` for the fixture's GitHub project, and the
# stand-in Claude CLI and `mcp` poster of the `tasks-mcp` group, so the cases call the issue tools as the agent of
# the fixture's thread.
source "$(dirname "${BASH_SOURCE[0]}")/issues-fixtures/setup.sh"
install_gh
workspace_origin https://github.com/acme/calc.git
source "$(dirname "${BASH_SOURCE[0]}")/tasks-mcp.sh"

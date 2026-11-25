# Prepares the folder of the `issues-gitlab-mcp` RPC group: a signed-in `glab` for the fixture's GitLab project, and
# the stand-in Claude CLI and `mcp` poster of the `tasks-mcp` group, so the cases call the issue and pull request
# tools as the agent of the fixture's thread.
source "$(dirname "${BASH_SOURCE[0]}")/gitlab-fixtures/setup.sh"
install_glab
workspace_origin https://gitlab.com/group/one.git
source "$(dirname "${BASH_SOURCE[0]}")/tasks-mcp.sh"

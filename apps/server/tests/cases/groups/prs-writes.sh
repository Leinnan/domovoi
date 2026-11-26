# Prepares the folder of the `prs-writes` RPC group: a signed-in `gh` with admin access to the project's repository.
source "$(dirname "${BASH_SOURCE[0]}")/prs-fixtures/setup.sh"
install_gh
workspace_origin https://github.com/acme/calc.git

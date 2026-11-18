# Prepares the folder of the `prs-guides` RPC group: a signed-in `gh`, and a stand-in Codex that writes review guides.
source "$(dirname "${BASH_SOURCE[0]}")/prs-fixtures/setup.sh"
install_gh
workspace_origin https://github.com/acme/calc.git

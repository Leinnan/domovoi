# Prepares the folder of the `prs-nocheckout` RPC group: a signed-in `gh`, and no project on GitHub.
source "$(dirname "${BASH_SOURCE[0]}")/prs-fixtures/setup.sh"
install_gh
checkout gl-one https://gitlab.com/group/one.git

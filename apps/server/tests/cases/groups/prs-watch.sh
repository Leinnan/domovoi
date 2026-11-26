# Prepares the folder of the `prs-watch` RPC group: a signed-in `gh` answering for the fixture's GitHub project. The
# cases have a thread follow pull request 11 and change what `gh` says of it (its checks, its head, its review, its
# merge) between the watcher's cycles, which the group's environment paces at a fraction of a second.
source "$(dirname "${BASH_SOURCE[0]}")/prs-fixtures/setup.sh"
install_gh
workspace_origin https://github.com/acme/calc.git

# pr-set <jq program>: rewrites what `gh` says of pull request 11.
cat > "$DIR/bin/pr-set" <<'SH'
#!/bin/bash
f="$(cd "$(dirname "$0")/.." && pwd)/gh-data/prs/acme_calc/11.json"
jq "$1" "$f" > "$f.next" && mv "$f.next" "$f"
SH
chmod +x "$DIR/bin/pr-set"

# Prepares the folder of the `issues-trackers` RPC group: a signed-in `gh` for the fixture's GitHub project, and a
# stand-in for Linear's API on the port the group's `DOMOVOI_LINEAR_API_URL` names (`issues-fixtures/linear.py`).
source "$(dirname "${BASH_SOURCE[0]}")/issues-fixtures/setup.sh"
install_gh
workspace_origin https://github.com/acme/calc.git
checkout plain
nohup /usr/bin/python3 "$FIXTURES/linear.py" "$DIR" 47613 > /dev/null 2>&1 &
for _ in $(seq 100); do [ -f "$DIR/linear-ready" ] && break; sleep 0.1; done
[ -f "$DIR/linear-ready" ]

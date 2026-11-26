# Stand-ins for `gh` and `ssh` (see repos-lib.sh), the hosted repositories a publish pushes
# to, and the checkouts that are published.
source "$(dirname "${BASH_SOURCE[0]}")/repos-lib.sh"

for name in pub pub-public pub-two pub-denied; do
  git init -q --bare "$DIR/remotes/acme/$name.git"
done

# A checkout with one commit on `main` (or the branch given).
checkout() {
  git init -q -b "${2:-main}" "$DIR/work/$1"
  echo "# $1" > "$DIR/work/$1/README.md"
  git -C "$DIR/work/$1" add -A
  git -C "$DIR/work/$1" commit -qm init
}
for name in main denied denied-ssh hostkey nowhere badname partial errors tolerant; do
  checkout "$name"
done
checkout origins feature/x
git -C "$DIR/work/origins" remote add origin https://example.test/acme/other.git
git -C "$DIR/work/origins" remote add origin-2 https://example.test/acme/another.git
checkout detached
git -C "$DIR/work/detached" checkout -q --detach
git init -q -b main "$DIR/work/unborn"
git init -q -b trunk "$DIR/work/unborn-trunk"
mkdir -p "$DIR/work/plain"
echo "no repository here" > "$DIR/work/plain/notes.txt"

# Stand-ins for `gh` and `ssh`, and the repository clones are made of (see repos-lib.sh).
source "$(dirname "${BASH_SOURCE[0]}")/repos-lib.sh"

mkdir -p "$DIR/clones/full" "$DIR/ssh-says"
echo "kept" > "$DIR/clones/full/notes.txt"

# What the stand-in ssh prints for `git@<name>.says.test`, where git's progress comes.
printf 'Receiving objects:  45%% (450/1000), 1.20 MiB | 2.00 MiB/s\r' > "$DIR/ssh-says/receiving"
printf 'remote: Counting objects:  50%% (1/2)\r' > "$DIR/ssh-says/counting-hang"
printf 'Resolving deltas:  10%% (1/10)\rResolving deltas: some of them\r' > "$DIR/ssh-says/resolving-hang"
# A detail longer than a snapshot keeps, and more to say about the failure than it keeps.
long=$(printf 'x%.0s' $(seq 1 1200))
{
  printf 'Updating files:  99%% (99/100), %s\r' "$long"
  for i in 1 2 3 4 5; do
    printf 'error %s: %s\n' "$i" "$long"
  done
} > "$DIR/ssh-says/long"

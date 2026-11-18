# Something for a storage cleanup to find: a log older than two weeks beside a
# new one, and a folder under Domovoi's worktrees that no thread uses. `claude` is
# a stand-in that fails at once, or hangs while `claude-hangs` exists (a run
# that stays active).

cat > "$DIR/bin/claude" <<'SH'
#!/bin/bash
here="$(cd "$(dirname "$0")/.." && pwd)"
[ -e "$here/claude-hangs" ] && exec sleep 60
exit 1
SH
chmod +x "$DIR/bin/claude"

# Makes a log file that was last written the given number of days ago.
cat > "$DIR/old-log" <<'SH'
#!/bin/bash
here="$(cd "$(dirname "$0")" && pwd)"
file="$here/home/userdata/logs/$1"
printf '%s' "$3" > "$file"
touch -t "$(date -v-"$2"d +%Y%m%d%H%M 2>/dev/null || date -d "$2 days ago" +%Y%m%d%H%M)" "$file"
SH
chmod +x "$DIR/old-log"

mkdir -p "$DIR/home/userdata/logs" "$DIR/home/worktrees/elsewhere/left-behind"
echo x > "$DIR/home/worktrees/elsewhere/left-behind/file.txt"
"$DIR/old-log" old.log 30 xyz
echo new > "$DIR/home/userdata/logs/new.log"

# The project's repository. A worktree's size counts its `.git` file, which
# names the repository's real path: that path is padded to one length whatever
# the length of this folder's name, and `repo` links to it.
pad="$(printf '%*s' $((150 - ${#DIR})) '' | tr ' ' p)"
mkdir -p "$DIR/$pad/repo"
ln -s "$pad/repo" "$DIR/repo"
git -C "$DIR/repo" init -q -b main
printf 'def add(a, b):\n    return a + b\n' > "$DIR/repo/calc.py"
git -C "$DIR/repo" add -A
git -C "$DIR/repo" -c user.name=t -c user.email=t@t commit -qm init

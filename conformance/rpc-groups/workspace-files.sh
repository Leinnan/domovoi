# A workspace with everything a listing, a search, a read or a write can meet:
# nested folders, ignored and hidden entries, symlinks in and out of the root,
# a binary file, one that is not UTF-8, one over the read limit, a nested worktree.
cat > userhome/.gitconfig <<'CFG'
[user]
	name = t
	email = t@t
[init]
	defaultBranch = main
CFG
mkdir -p userhome/Projects/alpha userhome/Projects/beta userhome/.cache
printf 'home notes\n' > userhome/notes.txt

mkdir -p outside
printf 'not for the workspace\n' > outside/secret.txt
mkfifo outside/pipe

mkdir -p tree/src/nested/deep/more tree/docs tree/build tree/.config tree/Assets
cd tree
git init -q -b main
printf 'build/\n*.log\nsecret.env\nwt-feature/\nbare.git/\n' > .gitignore
printf '# Tree\n\nAdd things here.\n' > README.md
printf 'def add(a, b):\n    return a + b\n' > calc.py
printf 'fn main() {\n    let add = 1; // addition\n}\n' > src/main.rs
printf 'pub fn add_two() {}\nconst \303\211T\303\211: &str = "\303\251t\303\251 add";\n' > src/nested/lib.rs
printf 'leaf\n' > src/nested/deep/more/leaf.txt
printf '{ "theme": "dark" }\n' > .config/settings.json
printf 'hidden\n' > .hidden
git add -A
git commit -qm init
git worktree add -q wt-feature -b feature
printf 'pub fn only_here() {}\n' > wt-feature/src/only.rs

# A line longer than what a match shows (512 characters), with a match inside and one beyond.
{ printf 'start needle '; head -c 600 /dev/zero | tr '\0' 'x'; printf ' needle end\n'; } > docs/guide.md
# More matching lines than one file gives (100).
i=1; while [ $i -le 130 ]; do printf 'zebra %03d\n' $i; i=$((i+1)); done > docs/zebras.txt
printf '\211PNG\r\n\032\n0123' > logo.PNG
printf '\211PNG\r\n\032\n0123' > Assets/icon.png
printf '\211PNG\r\n\032\n0123' > blob
printf '%%PDF-1.7 text' > paper
printf 'plain words' > words
: > empty
: > empty.txt
printf 'add before\n\000\001\002 add after the zero\n' > data.bin
printf 'caf\351 add latin\n' > latin1.txt
printf '\377\376\375' > notutf8.dat
head -c 1048576 /dev/zero | tr '\0' 'a' > big.txt
printf 'tail add\n' >> big.txt
printf 'add' > build/out.txt
printf 'add add' > debug.log
printf 'KEY=1\n' > secret.env
mkdir rodir
chmod 555 rodir
printf 'no access\n' > noaccess.txt
chmod 000 noaccess.txt
ln -s README.md link-file
ln -s src link-dir
ln -s ../outside link-out
ln -s ../outside/secret.txt link-out-file
ln -s . link-loop
ln -s missing link-broken
git init -q --bare bare.git
cd ..

# A folder that is no repository.
mkdir -p plain/sub
printf 'plain add\n' > plain/a.txt
printf 'x.log\n' > plain/x.log

# More ignored files than the list of them holds (200).
mkdir -p ignored-many
( cd ignored-many && git init -q -b main && printf '*.tmp\n' > .gitignore && i=1 && while [ $i -le 205 ]; do printf 'x' > f$(printf '%03d' $i).tmp; i=$((i+1)); done && printf 'kept\n' > kept.txt )

# A project whose folder goes away.
mkdir -p gone

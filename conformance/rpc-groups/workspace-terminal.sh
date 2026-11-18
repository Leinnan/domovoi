# The shell every terminal of this group runs: it says where it started and
# with what, then answers each line with one line. Input is not echoed and
# every answer is one write, so a terminal's history and the count of its
# events are the same on every run.
cat > bin/fakeshell <<'SH'
#!/bin/sh
stty -echo 2>/dev/null
printf 'READY args=%s cwd=%s thread=%s term=%s greeting=%s\n' "$*" "$PWD" "$T3CODE_THREAD_ID" "$TERM" "${GREETING-unset}"
while IFS= read -r line; do
  case "$line" in
    exit*) exit 3 ;;
    size) printf 'size:%s\n' "$(stty size)" ;;
    *) printf 'got:%s\n' "$line" ;;
  esac
done
SH
chmod +x bin/fakeshell
mkdir -p work

# Claude and Codex transcripts for several folders, in the home the server scans ($DIR/agent-home).
/usr/bin/python3 - <<'EOF'
import json, os, sqlite3, subprocess, time
d = os.environ["DIR"]
home, work, workspace = d + "/agent-home", d + "/work", d + "/workspace"
now = time.time()

def write(path, records, hours_ago):
    """A transcript last written `hours_ago` hours ago (the scan orders folders by that)."""
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        for r in records: f.write((r if isinstance(r, str) else json.dumps(r)) + "\n")
    os.utime(path, (now - hours_ago * 3600, now - hours_ago * 3600))

def claude(cwd, session, hours_ago, turns, extra=(), name=None):
    folder = "".join(c if c.isalnum() else "-" for c in cwd)
    records = [{"type": "summary", "summary": "not a message"}]
    for i, (prompt, reply) in enumerate(turns):
        at = "2025-03-01T10:%02d:" % i
        records.append({"type": "user", "cwd": cwd, "sessionId": session, "gitBranch": "main", "timestamp": at + "00Z", "origin": {"kind": "human"},
                        "message": {"role": "user", "content": prompt}})
        records.append({"type": "assistant", "cwd": cwd, "sessionId": session, "timestamp": at + "07.250Z",
                        "message": {"role": "assistant", "model": "claude-haiku-5-5", "content": [{"type": "thinking", "thinking": "hm"}, {"type": "text", "text": reply}]}})
    records.extend(extra)
    write("%s/.claude/projects/%s/%s.jsonl" % (home, folder, name or session), records, hours_ago)

def codex(cwd, session, hours_ago, prompt, reply, source="cli", day="2025/03/01", rollout=None):
    name = "rollout-2025-03-01T09-00-00-%s%s.jsonl" % (session, "_" + rollout if rollout else "")
    write("%s/.codex/sessions/%s/%s" % (home, day, name), [
        {"timestamp": "2025-03-01T09:00:00Z", "type": "session_meta", "payload": {"id": session, "cwd": cwd, "source": source, "cli_version": "0.40.0"}},
        {"type": "turn_context", "payload": {"cwd": cwd, "model": "gpt-5-codex"}},
        {"timestamp": "2025-03-01T09:00:01Z", "type": "response_item", "payload": {"type": "message", "role": "user",
            "content": [{"type": "input_text", "text": "<environment_context>\n  <cwd>%s</cwd>\n</environment_context>" % cwd}]}},
        {"timestamp": "2025-03-01T09:00:02Z", "type": "response_item", "payload": {"type": "message", "role": "user",
            "content": [{"type": "input_text", "text": prompt}], "internal_chat_message_metadata_passthrough": {"turn_id": "turn-1"}}},
        {"timestamp": "2025-03-01T09:00:02Z", "type": "event_msg", "payload": {"type": "user_message", "message": prompt}},
        {"type": "response_item", "payload": {"type": "reasoning", "summary": [], "content": None}},
        {"timestamp": "2025-03-01T09:00:09Z", "type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": reply}]}},
    ], hours_ago)

def git(folder, *args):
    subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", *args], cwd=folder, check=True, capture_output=True)

def repo(name, remote=None):
    folder = work + "/" + name
    os.makedirs(folder)
    git(folder, "init", "-q", "-b", "main")
    if remote: git(folder, "remote", "add", "origin", remote)
    return folder

# The fixture's project: two Claude sessions, a Codex one, and what an import leaves out.
claude(workspace, "11111111-1111-4111-8111-111111111111", 1, [("Where is add defined?", "In calc.py."), ("<system-reminder>be brief</system-reminder>And mul?", "There is none.")],
       extra=[{"type": "assistant", "isSidechain": True, "message": {"role": "assistant", "content": [{"type": "text", "text": "from a subagent"}]}},
              {"type": "user", "timestamp": "2025-03-01T10:30:00Z", "origin": {"kind": "task-notification"}, "message": {"role": "user", "content": "a background task finished"}},
              {"type": "assistant", "message": {"role": "assistant", "model": "<synthetic>", "content": [{"type": "text", "text": "API error"}]}},
              {"type": "ai-title", "aiTitle": "Find add"}, {"type": "custom-title", "customTitle": "  Where add lives  "}, "not json at all"])
claude(workspace, "22222222-2222-4222-8222-222222222222", 2, [("first", "reply 0")] + [("again %d" % i, "reply %d" % i) for i in range(1, 130)])
claude(workspace, "not-a-session-id", 3, [("A session Claude could not resume", "No.")])
claude(workspace, "11111111-1111-4111-8111-111111111111", 4, [("The same session in a second file", "Yes.")], name="33333333-3333-4333-8333-333333333333")
claude(workspace, "44444444-4444-4444-8444-444444444444", 5, [], extra=[{"type": "assistant", "cwd": workspace, "message": {"role": "assistant", "content": "nobody asked"}}])
codex(workspace, "019a1b2c-3d4e-7f60-8a1b-000000000001", 6, "Fix the tests", "Done.")
codex(workspace, "019a1b2c-3d4e-7f60-8a1b-000000000002", 7, "A subagent's work", "Done.", source={"subagent": {"parent": "x"}})
claude(workspace, "55555555-5555-4555-8555-555555555555", 24 * 45, [("Long ago", "Yes.")])

# A repository with a remote, sessions of both agents, and a linked worktree whose session counts toward it.
alpha = repo("alpha", "git@github.com:Acme/Alpha.git")
open(alpha + "/README.md", "w").write("# alpha\n")
git(alpha, "add", "-A"); git(alpha, "commit", "-qm", "init")
git(alpha, "worktree", "add", "-q", "-b", "side", work + "/alpha-side")
claude(alpha, "66666666-6666-4666-8666-666666666666", 8, [("What does alpha do?", "Little.")])
codex(alpha, "019a1b2c-3d4e-7f60-8a1b-000000000003", 9, "## My request for Codex:\nRename alpha", "Renamed.")
claude(work + "/alpha-side", "77777777-7777-4777-8777-777777777777", 0.5, [("Work in the worktree", "Ok.")])
# No repository; last touched long ago.
os.makedirs(work + "/beta")
claude(work + "/beta", "88888888-8888-4888-8888-888888888888", 10, [("Plain folder", "Yes.")])
os.utime(home + "/.claude/projects/" + "".join(c if c.isalnum() else "-" for c in work + "/beta") + "/88888888-8888-4888-8888-888888888888.jsonl", (1735787045, 1735787045))
# A repository without a remote, known to Codex only; one chat of it sits in Codex's scratch folder.
gamma = repo("gamma")
codex(gamma, "019a1b2c-3d4e-7f60-8a1b-000000000004", 11, "Start gamma", "Started.")
scratch = home + "/Documents/Codex/2025-03-01/chat"
os.makedirs(scratch)
codex(scratch, "019a1b2c-3d4e-7f60-8a1b-000000000005", 12, "A chat that belongs to gamma", "Yes.")
db = sqlite3.connect(home + "/.codex/state_5.sqlite")
db.executescript("CREATE TABLE threads (id TEXT, project_id TEXT); CREATE TABLE project_roots (project_id TEXT, path TEXT, position INTEGER);")
db.execute("INSERT INTO threads VALUES ('019a1b2c-3d4e-7f60-8a1b-000000000005', 'p-gamma')")
db.execute("INSERT INTO project_roots VALUES ('p-gamma', ?, 0)", (gamma,))
db.commit(); db.close()
# An https remote with a deeper path.
delta = repo("delta", "https://user@GitLab.example.com/group/sub/Repo.git")
claude(delta, "99999999-9999-4999-8999-999999999999", 13, [("Deeper", "Yes.")])
# Never offered: a folder that is gone, the home itself, downloads, a relative folder, no folder at all.
claude(work + "/gone", "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", 14, [("Gone", "Yes.")])
claude(home, "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", 15, [("Home", "Yes.")])
os.makedirs(home + "/Downloads/unpacked")
claude(home + "/Downloads/unpacked", "cccccccc-cccc-4ccc-8ccc-cccccccccccc", 16, [("Downloads", "Yes.")])
claude("relative/folder", "dddddddd-dddd-4ddd-8ddd-dddddddddddd", 17, [("Relative", "Yes.")])
write(home + "/.claude/projects/-nowhere/eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee.jsonl", [{"type": "user", "message": {"role": "user", "content": "no folder"}}], 18)
write(home + "/.claude/projects/-nowhere/empty.jsonl", [], 19)
open(home + "/.codex/session_index.jsonl", "w").write(json.dumps({"id": "019a1b2c-3d4e-7f60-8a1b-000000000001", "thread_name": "Codex rename"}) + "\nnot json\n")
EOF

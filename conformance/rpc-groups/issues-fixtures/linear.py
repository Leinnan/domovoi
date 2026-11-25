#!/usr/bin/python3
# Stand-in for Linear's GraphQL API in the `issues-trackers` RPC group:
# `linear.py <folder> <port>` serves http://127.0.0.1:<port>/graphql, answering
# the operations domovoi-server sends (`apps/server/src/rpc/issues/linear.rs`)
# by their names, for one workspace (`acme`) with the teams ENG and OPS.
#
# API keys: `lin_good` is accepted; `lin_bad` is refused (HTTP 401); `lin_limited`
# hits the rate limit (a RATELIMITED error with a reset time). While
# `<folder>/linear-revoked` exists every key is refused. Every request is
# appended to `<folder>/linear-log`. It stops after two idle minutes, when its
# folder is gone, or on `POST /shutdown` (a later run's stand-in asks it to).
import json, os, sys, threading, time, urllib.request
from http.server import BaseHTTPRequestHandler, HTTPServer

folder, port = sys.argv[1], int(sys.argv[2])
last = [time.time()]

def user(id, handle, name):
    return {"id": id, "name": name, "displayName": handle, "email": handle + "@acme.test", "avatarUrl": None}

ME, ANN = user("u-me", "me", "Me Myself"), user("u-ann", "ann", "Ann")
STATES = [
    {"id": "s-done", "name": "Done", "type": "completed", "position": 0},
    {"id": "s-todo", "name": "Todo", "type": "unstarted", "position": 1},
    {"id": "s-back", "name": "Backlog", "type": "backlog", "position": 0},
    {"id": "s-prog", "name": "In Progress", "type": "started", "position": 0},
    {"id": "s-cancel", "name": "Canceled", "type": "canceled", "position": 0},
]
STATE = {s["id"]: s for s in STATES}
LABELS = [{"id": "l-bug", "name": "Bug", "color": "#eb5757", "description": "Broken"}]
WORKSPACE_LABELS = [{"id": "l-ux", "name": "UX", "color": "#5e6ad2", "description": None}]
PAGE = {"hasNextPage": False, "endCursor": None}

def comment(id, who, body, at, reactions=()):
    return {"id": id, "body": body, "createdAt": at, "url": "https://linear.app/acme/issue/ENG-1#comment-" + id, "user": who, "botActor": None, "reactions": list(reactions)}

ISSUES = {
    "ENG-1": {"id": "i-1", "identifier": "ENG-1", "number": 1, "title": "Login button does nothing", "url": "https://linear.app/acme/issue/ENG-1/login-button",
              "createdAt": "2026-11-01T09:00:00.000Z", "updatedAt": "2026-11-23T09:00:00.000Z", "completedAt": None, "canceledAt": None,
              "state": STATE["s-prog"], "team": {"key": "ENG"}, "creator": ANN, "botActor": None, "assignee": ME, "labels": {"nodes": [LABELS[0]]},
              "description": "Clicking it shows nothing.",
              "reactions": [{"id": "r-1", "emoji": "+1", "user": ME}, {"id": "r-2", "emoji": "\U0001F44D", "user": ANN}, {"id": "r-3", "emoji": "\U0001F937", "user": ANN}],
              "attachments": {"nodes": [
                  {"url": "https://github.com/acme/calc/pull/12", "title": "Fix the login button", "sourceType": "github", "metadata": {"status": "draft"}},
                  {"url": "https://github.com/acme/calc/pull/12", "title": "Fix the login button", "sourceType": "github", "metadata": {"status": "draft"}},
                  {"url": "https://gitlab.com/acme/web/-/merge_requests/4", "title": "", "sourceType": "gitlab", "metadata": {"status": "merged"}},
                  {"url": "https://www.figma.com/file/x", "title": "Mockup", "sourceType": "figma", "metadata": {}}]},
              "history": {"nodes": [
                  {"id": "h-1", "createdAt": "2026-11-02T09:00:00.000Z", "actor": ANN, "botActor": None, "fromState": STATE["s-todo"], "toState": STATE["s-prog"],
                   "fromAssignee": None, "toAssignee": ME, "addedLabels": [LABELS[0]], "removedLabels": [], "fromTitle": "Login broken", "toTitle": "Login button does nothing"},
                  {"id": "h-2", "createdAt": "2026-11-03T09:00:00.000Z", "actor": None, "botActor": {"name": "github", "userDisplayName": "GitHub", "avatarUrl": None},
                   "fromState": STATE["s-prog"], "toState": STATE["s-done"], "fromAssignee": None, "toAssignee": None, "addedLabels": None, "removedLabels": None, "fromTitle": None, "toTitle": None},
                  {"id": "h-3", "createdAt": "2026-11-04T09:00:00.000Z", "actor": ME, "botActor": None, "fromState": STATE["s-done"], "toState": STATE["s-prog"],
                   "fromAssignee": ANN, "toAssignee": ME, "addedLabels": [], "removedLabels": [LABELS[0]], "fromTitle": None, "toTitle": None}]},
              "comments": [comment("c-1", ANN, "Seen on Safari.", "2026-11-01T10:00:00.000Z", [{"id": "r-4", "emoji": "heart", "user": ME}]),
                           comment("c-2", ME, "Looking.", "2026-11-01T11:00:00.000Z")]},
    "ENG-2": {"id": "i-2", "identifier": "ENG-2", "number": 2, "title": "Dark mode", "url": "https://linear.app/acme/issue/ENG-2/dark-mode",
              "createdAt": "2026-10-01T09:00:00.000Z", "updatedAt": "2026-10-02T09:00:00.000Z", "completedAt": "2026-10-02T09:00:00.000Z", "canceledAt": None,
              "state": STATE["s-done"], "team": {"key": "ENG"}, "creator": ME, "botActor": None, "assignee": None, "labels": {"nodes": []},
              "description": None, "reactions": [], "attachments": {"nodes": []}, "history": {"nodes": []},
              "comments": [comment("c-9", ANN, "Shipped.", "2026-10-02T09:00:00.000Z")]},
    "ENG-7": {"id": "i-7", "identifier": "ENG-7", "number": 7, "title": "Moved to operations", "url": "https://linear.app/acme/issue/ENG-7/moved",
              "createdAt": "2026-09-01T09:00:00.000Z", "updatedAt": "2026-09-02T09:00:00.000Z", "completedAt": None, "canceledAt": None,
              "state": STATE["s-todo"], "team": {"key": "OPS"}, "creator": None, "botActor": {"name": "forms", "userDisplayName": None, "avatarUrl": None},
              "assignee": None, "labels": {"nodes": []}, "description": "", "reactions": [], "attachments": {"nodes": []}, "history": {"nodes": []}, "comments": []},
    "OPS-1": {"id": "i-o1", "identifier": "OPS-1", "number": 1, "title": "Rotate keys", "url": "https://linear.app/acme/issue/OPS-1/rotate",
              "createdAt": "2026-08-01T09:00:00.000Z", "updatedAt": "2026-08-02T09:00:00.000Z", "completedAt": None, "canceledAt": "2026-08-02T09:00:00.000Z",
              "state": STATE["s-cancel"], "team": {"key": "OPS"}, "creator": ME, "botActor": None, "assignee": None, "labels": {"nodes": []},
              "description": "", "reactions": [], "attachments": {"nodes": []}, "history": {"nodes": []}, "comments": []},
}
TEAMS = {"ENG": {"id": "t-eng", "key": "ENG", "name": "Engineering"}, "OPS": {"id": "t-ops", "key": "OPS", "name": "Operations"}}
TEMPLATES = [
    {"id": "tpl-bug", "name": "Bug", "description": "Something broke", "content": None, "type": "issue", "hasFormFields": False,
     "templateData": json.dumps({"title": "Bug: ", "description": "## Steps", "labelIds": ["l-bug", "l-gone"], "assigneeId": "u-ann"})},
    {"id": "tpl-form", "name": "Request", "description": None, "content": None, "type": "issue", "hasFormFields": True, "templateData": {}},
    {"id": "tpl-doc", "name": "Spec", "description": "", "content": "# Spec", "type": "document", "hasFormFields": False, "templateData": "{}"},
]

def summary(issue):
    return {k: v for k, v in issue.items() if k not in ("description", "reactions", "attachments", "history", "comments")}

def connection(nodes, first, after):
    start = int(after or 0)
    page = nodes[start:start + first]
    more = start + first < len(nodes)
    return {"nodes": page, "pageInfo": {"hasNextPage": more, "endCursor": str(start + first) if more else None}}

def closed(issue):
    return issue["state"]["type"] in ("completed", "canceled", "duplicate")

def matches(issue, f):
    if issue["team"]["key"] != f.get("team", {}).get("key", {}).get("eq"):
        return False
    t = f.get("state", {}).get("type", {})
    if "in" in t and issue["state"]["type"] not in t["in"]:
        return False
    if "nin" in t and issue["state"]["type"] in t["nin"]:
        return False
    for clause in f.get("and", []):
        if "assignee" in clause and (issue["assignee"] or {}).get("displayName") != clause["assignee"]["displayName"]["eq"]:
            return False
        if "labels" in clause and clause["labels"]["some"]["name"]["eq"] not in [l["name"] for l in issue["labels"]["nodes"]]:
            return False
    if "assignee" in f and (issue["assignee"] or {}).get("displayName") != f["assignee"]["displayName"]["eq"]:
        return False
    if "creator" in f and (issue["creator"] or {}).get("displayName") != f["creator"]["displayName"]["eq"]:
        return False
    if "or" in f:
        q = f["or"][0]["title"]["containsIgnoreCase"].lower()
        number = next((c["number"]["eq"] for c in f["or"] if "number" in c), None)
        if q not in issue["title"].lower() and q not in (issue["description"] or "").lower() and issue["number"] != number:
            return False
    return True

def answer(op, v):
    if op == "T3Viewer":
        return {"viewer": ME, "organization": {"urlKey": "acme"}, "teams": {"nodes": list(TEAMS.values())}}
    if op == "T3Issues":
        rows = [summary(i) for i in ISSUES.values() if matches(i, v["filter"])]
        rows.sort(key=lambda i: i[v["orderBy"]], reverse=True)
        return {"issues": connection(rows, v["first"], v.get("after"))}
    if op in ("T3Issue", "T3Comments", "T3IssueReactions"):
        issue = ISSUES.get(v["id"]) or next((i for i in ISSUES.values() if i["id"] == v["id"]), None)
        if issue is None:
            return None
        if op == "T3IssueReactions":
            return {"issue": {"reactions": issue["reactions"]}}
        if op == "T3Comments":
            return {"issue": {"comments": connection(issue["comments"], v["first"], v.get("after"))}}
        detail = dict(issue, team=dict(issue["team"], states={"nodes": STATES}), comments=connection(issue["comments"], v["comments"], None))
        return {"issue": detail}
    if op == "T3CommentReactions":
        for issue in ISSUES.values():
            for c in issue["comments"]:
                if c["id"] == v["id"]:
                    return {"comment": {"issue": {"id": issue["id"]}, "reactions": c["reactions"]}}
        return None
    if op == "T3Team":
        team = TEAMS.get(v["key"])
        nodes = [] if team is None else [dict(team, states={"nodes": STATES, "pageInfo": PAGE}, labels={"nodes": LABELS, "pageInfo": PAGE},
                                              members={"nodes": [ME, ANN], "pageInfo": {"hasNextPage": True, "endCursor": "m"}}, templates={"nodes": TEMPLATES})]
        return {"teams": {"nodes": nodes}, "issueLabels": {"nodes": WORKSPACE_LABELS, "pageInfo": PAGE}}
    if op == "T3IssueCreate":
        return {"issueCreate": {"success": True, "issue": {"identifier": "ENG-3", "url": "https://linear.app/acme/issue/ENG-3/new"}}}
    for name, field in (("T3IssueUpdate", "issueUpdate"), ("T3CommentCreate", "commentCreate"), ("T3ReactionCreate", "reactionCreate"), ("T3ReactionDelete", "reactionDelete")):
        if op == name:
            return {field: {"success": True}}
    raise ValueError(op)

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def send(self, status, body, headers=()):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        for k, v in headers:
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        last[0] = time.time()
        body = self.rfile.read(int(self.headers.get("content-length") or 0))
        if self.path == "/shutdown":
            self.send(200, {})
            threading.Thread(target=server.shutdown).start()
            return
        request = json.loads(body or b"{}")
        query, v = request.get("query", ""), request.get("variables") or {}
        op = query.split("(")[0].split("{")[0].split()[1]
        token = self.headers.get("authorization")
        with open(os.path.join(folder, "linear-log"), "a") as log:
            log.write(json.dumps({"op": op, "authorization": token, "variables": v}) + "\n")
        if token == "lin_bad" or os.path.exists(os.path.join(folder, "linear-revoked")):
            return self.send(401, {"errors": [{"message": "Authentication required, not authenticated", "extensions": {"code": "AUTHENTICATION_ERROR"}}]})
        if token == "lin_limited":
            reset = str(int(time.time() * 1000) + 60_000)
            return self.send(400, {"errors": [{"message": "Rate limit exceeded", "extensions": {"code": "RATELIMITED"}}]}, [("x-ratelimit-requests-reset", reset)])
        if token != "lin_good":
            return self.send(401, {"errors": [{"message": "Invalid API key", "extensions": {"code": "AUTHENTICATION_ERROR"}}]})
        data = answer(op, v)
        if data is None:
            return self.send(200, {"errors": [{"message": "Entity not found: Issue", "extensions": {"code": "INVALID_INPUT"}}], "data": None})
        self.send(200, {"data": data})

def watch():
    while True:
        time.sleep(1)
        if time.time() - last[0] > 120 or not os.path.isdir(folder):
            server.shutdown()
            return

for attempt in range(50):
    try:
        server = HTTPServer(("127.0.0.1", port), Handler)
        break
    except OSError:
        # A stand-in of an earlier run still holds the port: ask it to go.
        try:
            urllib.request.urlopen(urllib.request.Request("http://127.0.0.1:%d/shutdown" % port, data=b"", method="POST"), timeout=1)
        except Exception:
            pass
        time.sleep(0.2)
else:
    sys.exit(1)
open(os.path.join(folder, "linear-ready"), "w").close()
threading.Thread(target=watch, daemon=True).start()
server.serve_forever()

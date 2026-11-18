# Golden fixtures

Traffic recorded from the real t3code TypeScript server (`pingdotgg/t3code@f313d0a`,
Effect 4.1.3) driven by its unmodified web client. These are the compatibility target
for `domovoi-server`.

## ts-session1

| File | Contents |
|---|---|
| `wire.ndjson` | Everything between the web client and the server, recorded by `apps/ws-tap`: `http` exchanges, `ws-open`/`ws-close`, and one `ws` record per WebSocket frame (`dir` is `c2s` or `s2c`, `frame` is the parsed Effect RPC message). `marker` records split the session into segments. |
| `provider-native.ndjson` | The server's own provider logs: the native `codex app-server` JSON-RPC exchange (`protocol: codex.app-server`) and, for Claude, the Agent SDK message stream (`protocol: claude-agent-sdk.query`). The SDK log does not show the CLI's control protocol. |
| `claude-cli.args.txt`, `claude-cli.stdin.ndjson`, `claude-cli.stdout.ndjson` | Raw stdio between the Agent SDK and the `claude` CLI for claude-turn-3, captured by a wrapper set as the Claude binary path. This is what a Rust adapter must speak: launch flags, `control_request` `initialize` / `mcp_set_servers`, the `user` message, `stream_event`s, and the `can_use_tool` request with its `allow` response. stdin and stdout were captured separately, so relative ordering between them is not recorded. |

Segments (`marker` labels):

1. **setup**: pairing, browser-session cookie, socket upgrade (including a 401 before the cookie lands), boot subscriptions, adding a project by path.
2. **codex-turn-1**: `orchestration.launchThread` on GPT-6-Luna in Supervised mode. Two `item/commandExecution/requestApproval` round trips answered with `runtime-request.respond`. The turn ends without edits (Supervised maps to a read-only sandbox).
3. **codex-turn-2**: `thread.runtime-mode.set` to auto-accept edits, then `message.dispatch`. One file is edited and a checkpoint is created.
4. **checkpoint-revert**: `checkpoint.rollback` with file restore (fails: a thread on the current checkout is not an isolated worktree), then without it (succeeds; native `thread/revert`).
5. **terminal**: `terminal.open`, `attach`, `resize`, `write`.
6. **claude setup**: provider status flips from unauthenticated to authenticated after `claude auth login` and a refresh.
7. **claude-turn-1**: new thread on Claude Haiku 5.5, Supervised. One Edit approval, one file changed, checkpoint.
8. **claude-turn-2**: follow-up on the same thread, reusing its live CLI process. One Edit approval.
9. **claude-turn-3**: new thread whose CLI process went through the stdio wrapper (the `claude-cli.*` files). One Edit approval.

## Regenerating

Run the TS server with an isolated `--home-dir`, put `ws-tap` between Vite's proxy and the
server (`dev:web --port <tap port>`), and drive the client. Then:

```sh
scripts/scrub_recording.py --tap <rec.ndjson> --provider-log <home>/userdata/logs/provider/events.<thread>.log \
  --raw claude-cli.stdin.ndjson=<claude-tap log>.stdin \
  --home "$HOME" --redact "$(hostname -s)=host" --out fixtures/golden/<name>
scripts/check_fixtures.py fixtures --forbid <pairing token> --forbid <account ids>
```

Never commit a fixture that `check_fixtures.py` rejects.

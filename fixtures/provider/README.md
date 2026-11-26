# Provider protocol recordings

What provider CLIs were started with, sent and received in one recorded
session. They show the protocols the server's adapters speak.

| File | Contents |
|---|---|
| `claude-cli.args.txt`, `claude-cli.stdin.ndjson`, `claude-cli.stdout.ndjson` | Raw stdio between the Claude Agent SDK and the `claude` CLI for one turn, captured by a wrapper set as the Claude binary path: launch flags, `control_request` `initialize` and `mcp_set_servers`, the `user` message, `stream_event`s, and the `can_use_tool` request with its `allow` response. stdin and stdout were captured separately, so their order relative to each other is not recorded. `apps/server/src/claude_cli.rs` tests its launch arguments and first messages against them. |
| `provider-native.ndjson` | Reference only, read by no test: the native `codex app-server` JSON-RPC exchange (`protocol: codex.app-server`) and the Claude Agent SDK's message stream (`protocol: claude-agent-sdk.query`) of the same session. |

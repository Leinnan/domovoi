# Native fixtures

Traffic recorded from `domovoi-server` itself, between the server and a socket
that subscribes as the client core does. Tests and benches read these files
for realistic records: a server configuration, the thread list, and threads
with every kind of item.

## session1

`wire.ndjson`: one record per WebSocket frame, in the order they crossed:
`{"kind":"ws","conn":1,"dir":"c2s"|"s2c","frame":…}`.

The socket subscribes to the configuration, the lifecycle and the thread
list, then to each thread as it is opened:

- thread `t1`, on the scripted Codex: a command, a file change, a web search,
  a plan, a subagent, a question and approvals (accepted, accepted for the
  session, declined, cancelled), each answered;
- thread `t2`, on the scripted Claude: a reply with thinking, then a
  20,000-character reply streamed over about ten seconds;
- both threads subscribed to once more at the end, for their whole snapshots.

The requests that drive the turns go over a second socket and are not in the
recording.

## Recording again

After a change to what the server sends, record again and commit the result:

```sh
DOMOVOI_RECORD_NATIVE=$PWD/rec.ndjson cargo test -p domovoi-server --test provider_turns record_a_native_session -- --ignored
scripts/scrub_recording.py --tap rec.ndjson --home "$HOME" --redact "$(hostname -s)=host" \
  --redact "<the test's temp folder>=/tmp/domovoi-record" --out fixtures/native/session1
rm fixtures/native/session1/provider-native.ndjson
scripts/check_fixtures.py fixtures
```

The temp folder is the `domovoi-provider-turns-record-<pid>` path in the
recording (on macOS under `/var/folders/…/T`, also spelled with `/private` in
front: redact the longer spelling first). Never commit a fixture that
`check_fixtures.py` rejects.

Ids, times and the order of concurrent events differ from one recording to the
next. A test that reads this file must not depend on them.

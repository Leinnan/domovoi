# The providers' own session logs, as Claude Code and Codex write them, in the
# home the server reads them from. The Codex ones wait in `stash` until a case
# moves them in, so the first summary finds that source missing.
claude=agent-home/.claude/projects
mkdir -p "$claude/-Users-x-calc" "$claude/-Users-x-other/nested" stash/sessions/2025/03/10 stash/sessions/2025/03/11

assistant() { # id request timestamp session model input cache_read cache_creation output
  printf '{"parentUuid":"p","isSidechain":false,"type":"assistant","timestamp":"%s","sessionId":"%s","requestId":"%s","cwd":"/Users/x/calc","message":{"id":"%s","type":"message","role":"assistant","model":"%s","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":%s,"cache_read_input_tokens":%s,"cache_creation_input_tokens":%s,"output_tokens":%s,"service_tier":"standard"}}}\n' "$3" "$4" "$2" "$1" "$5" "$6" "$7" "$8" "$9"
}
{
  printf '{"type":"user","timestamp":"2025-03-10T09:59:00.000Z","sessionId":"s1","message":{"role":"user","content":"what is my usage?"}}\n'
  assistant msg_1 req_1 2025-03-10T10:00:00.000Z s1 claude-haiku-5-5 10 5 2 3
  # The same message streamed into a second record: counted once.
  assistant msg_1 req_1 2025-03-10T10:00:01.000Z s1 claude-haiku-5-5 10 5 2 3
  printf '{"usage": not json\n'
  assistant msg_2 req_2 2025-03-10T10:40:00.000Z s1 claude-haiku-5-5 100 50 20 30
  assistant msg_3 req_3 2025-03-10T23:30:00.000Z s1 claude-haiku-5-5 1 0 0 1
  assistant msg_4 req_4 2025-03-10T11:00:00.000Z s1 '<synthetic>' 0 0 0 0
  printf '{"type":"assistant","timestamp":"2025-03-10T11:05:00.000Z","sessionId":"s1","message":{"id":"msg_5","model":"claude-haiku-5-5","content":"no usage here, only the word \\"usage\\""}}\n'
  printf '{"type":"assistant","sessionId":"s1","requestId":"req_6","message":{"id":"msg_6","model":"claude-haiku-5-5","usage":{"input_tokens":7,"output_tokens":7}}}\n'
  assistant msg_7 req_7 2025-03-11T08:15:00.000Z s1 claude-opus-5 1000 2000 300 400
} > "$claude/-Users-x-calc/s1.jsonl"
{
  assistant msg_8 req_8 2025-03-10T10:20:00.000Z s2 claude-haiku-5-5 4 3 2 1
  printf '{"type":"assistant","timestamp":"2025-03-10T10:25:00.000Z","sessionId":"s2","message":{"usage":{"input_tokens":9,"output_tokens":1}}}\n'
} > "$claude/-Users-x-other/nested/s2.jsonl"
printf 'not a log\n' > "$claude/-Users-x-other/readme.txt"
# A log last written long before any day asked for but 2019's.
assistant msg_old req_old 2019-06-01T12:00:00.000Z s0 claude-old-1 11 0 0 22 > "$claude/-Users-x-calc/old.jsonl"
touch -t 202001010000 "$claude/-Users-x-calc/old.jsonl"

tokens() { # timestamp input cached output reasoning
  printf '{"timestamp":"%s","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":999,"cached_input_tokens":0,"output_tokens":999,"reasoning_output_tokens":0,"total_tokens":1998},"last_token_usage":{"input_tokens":%s,"cached_input_tokens":%s,"output_tokens":%s,"reasoning_output_tokens":%s,"total_tokens":0},"model_context_window":258400},"rate_limits":null}}\n' "$1" "$2" "$3" "$4" "$5"
}
{
  printf '{"timestamp":"2025-03-10T09:00:00.000Z","type":"session_meta","payload":{"id":"x","cwd":"/Users/x/calc","originator":"codex_cli_rs"}}\n'
  tokens 2025-03-10T09:00:02.000Z 5 0 5 0
  printf '{"timestamp":"2025-03-10T09:00:03.000Z","type":"turn_context","payload":{"cwd":"/Users/x/calc","model":"gpt-6-luna","effort":"medium"}}\n'
  tokens 2025-03-10T09:00:05.000Z 100 40 7 2
  printf '{"timestamp":"2025-03-10T09:00:06.000Z","type":"event_msg","payload":{"type":"token_count","info":null}}\n'
  tokens 2025-03-10T09:30:00.000Z 200 150 9 0
  printf '{"timestamp":"2025-03-10T10:00:00.000Z","type":"turn_context","payload":{"cwd":"/Users/x/calc","model":"gpt-6-mini"}}\n'
  tokens 2025-03-10T10:00:05.000Z 10 20 1 1
  printf '{"type":"turn_context" broken\n'
} > stash/sessions/2025/03/10/rollout-2025-03-10T09-00-00-aaaa.jsonl
{
  printf '{"timestamp":"2025-03-11T07:00:00.000Z","type":"turn_context","payload":{"model":"gpt-6-luna"}}\n'
  tokens 2025-03-11T07:00:05.000Z 50 10 5 5
} > stash/sessions/2025/03/11/rollout-2025-03-11T07-00-00-bbbb.jsonl
# Not a rollout: never read.
tokens 2025-03-11T07:00:05.000Z 1 1 1 1 > stash/sessions/2025/03/11/notes.jsonl

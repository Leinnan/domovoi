// Validates recorded WebSocket RPC traffic against t3code's real contracts.
//
// For every Request in a wire.ndjson (from apps/ws-tap or the golden fixtures),
// looks the method up in t3code's WsRpcGroup and decodes, with the same Effect
// schemas the web client uses:
//   - the request payload,
//   - every streamed Chunk value (stream success schema),
//   - the terminal Exit (Rpc.exitSchema).
// Usage: node conformance/wire-check.ts <wire.ndjson> [--quiet]
import { readFileSync } from "node:fs";
import * as Schema from "effect/Schema";
import * as Option from "effect/Option";
import * as Rpc from "effect/rpc/Rpc";
import * as RpcSchema from "effect/rpc/RpcSchema";
import { WsRpcGroup } from "../.reference/t3code/packages/contracts/src/rpc.ts";

const file = process.argv[2];
if (!file) throw new Error("usage: wire-check.ts <wire.ndjson>");
const quiet = process.argv.includes("--quiet");

type AnyRpc = Rpc.AnyWithProps;
const rpcs = (WsRpcGroup as unknown as { requests: ReadonlyMap<string, AnyRpc> }).requests;

const decoders = new Map<string, { payload: (u: unknown) => unknown; chunk?: (u: unknown) => unknown; exit: (u: unknown) => unknown }>();
const decodersFor = (tag: string) => {
  let d = decoders.get(tag);
  if (d) return d;
  const rpc = rpcs.get(tag);
  if (!rpc) return undefined;
  const json = <S extends Schema.Top>(s: S) => Schema.decodeUnknownSync(Schema.toCodecJson(s) as never);
  const stream = RpcSchema.getStreamSchemas(rpc.successSchema);
  d = {
    payload: json(rpc.payloadSchema),
    chunk: Option.isSome(stream) ? json(stream.value.success) : undefined,
    exit: json(Rpc.exitSchema(rpc as never)),
  };
  decoders.set(tag, d);
  return d;
};

const tags = new Map<string, string>(); // `${conn}:${id}` -> tag
const stats = new Map<string, { ok: number; fail: number }>();
const failures: string[] = [];
const bump = (tag: string, ok: boolean) => {
  const s = stats.get(tag) ?? { ok: 0, fail: 0 };
  ok ? s.ok++ : s.fail++;
  stats.set(tag, s);
};
const check = (tag: string, what: string, fn: () => unknown) => {
  try {
    fn();
    bump(tag, true);
  } catch (e) {
    bump(tag, false);
    failures.push(`${tag} ${what}: ${String((e as Error).message).split("\n").slice(0, 6).join(" | ")}`);
  }
};

for (const line of readFileSync(file, "utf8").split("\n")) {
  if (!line) continue;
  const rec = JSON.parse(line);
  if (rec.kind !== "ws" || typeof rec.frame !== "object" || rec.frame === null) continue;
  const f = rec.frame;
  if (f._tag === "Request") {
    tags.set(`${rec.conn}:${f.id}`, f.tag);
    const d = decodersFor(f.tag);
    if (!d) {
      bump(f.tag, false);
      failures.push(`${f.tag}: not in WsRpcGroup`);
      continue;
    }
    check(f.tag, "payload", () => d.payload(f.payload));
  } else if (f._tag === "Chunk" || f._tag === "Exit") {
    const tag = tags.get(`${rec.conn}:${f.requestId}`);
    if (!tag) continue;
    const d = decodersFor(tag)!;
    if (f._tag === "Chunk") {
      for (const v of f.values) check(tag, "chunk", () => (d.chunk ? d.chunk(v) : (() => { throw new Error("chunk for non-stream rpc"); })()));
    } else {
      check(tag, "exit", () => d.exit(f.exit));
    }
  }
}

if (!quiet) {
  for (const [tag, s] of [...stats].sort()) console.log(`${s.fail ? "FAIL" : "ok  "} ${tag} (${s.ok} ok, ${s.fail} failed)`);
}
for (const f of failures.slice(0, 40)) console.log("  -", f);
const failed = [...stats.values()].reduce((n, s) => n + s.fail, 0);
const total = [...stats.values()].reduce((n, s) => n + s.ok + s.fail, 0);
console.log(failed ? `WIRE CHECK FAILED: ${failed}/${total} messages` : `WIRE CHECK PASSED: ${total} messages, ${stats.size} methods`);
process.exit(failed ? 1 : 0);

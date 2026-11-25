// Drives the Rust effect-rpc server with Effect's real RpcClient over a real
// WebSocket, using the same layers t3code's client-runtime uses
// (makeProtocolSocket + RpcSerialization.layerJson + joinSocketFrameParts).
// Usage: node conformance/effect-client.ts ws://127.0.0.1:PORT/ws
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Cause from "effect/Cause";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";
import * as Socket from "effect/socket/Socket";
import * as Rpc from "effect/rpc/Rpc";
import * as RpcClient from "effect/rpc/RpcClient";
import * as RpcGroup from "effect/rpc/RpcGroup";
import * as RpcSerialization from "effect/rpc/RpcSerialization";
import { joinSocketFrameParts } from "../.reference/t3code/packages/shared/src/socketFrameParts.ts";

class Nope extends Schema.TaggedError<Nope>()("Nope", { message: Schema.String }) {}

const Group = RpcGroup.make(
  Rpc.make("echo", { payload: { a: Schema.Number }, success: Schema.Struct({ a: Schema.Number }) }),
  Rpc.make("fail", { error: Nope }),
  Rpc.make("sleep"),
  Rpc.make("big", { success: Schema.String }),
  Rpc.make("count", { payload: { n: Schema.Number }, success: Schema.Number, stream: true }),
);

const url = new URL(process.argv[2]!);
url.searchParams.set("frameParts", "1");
url.searchParams.set("orchestrationProtocol", "2");

const results: Array<[string, boolean, string?]> = [];
const check = (name: string, ok: boolean, detail?: string) => results.push([name, ok, detail]);

const program = Effect.gen(function* () {
  const client = yield* RpcClient.make(Group);

  const echoed = yield* client.echo({ a: 42 });
  check("unary success", echoed.a === 42, JSON.stringify(echoed));

  const failed = yield* Effect.exit(client.fail());
  const err = Exit.isFailure(failed) ? Cause.findErrorOption(failed.cause) : undefined;
  check(
    "typed failure decodes as Nope",
    err !== undefined && err._tag === "Some" && err.value instanceof Nope && err.value.message === "no",
    String(failed),
  );

  const counted = yield* client.count({ n: 300 }).pipe(Stream.runCollect);
  const arr = Array.from(counted);
  check("stream with acks delivers all items in order", arr.length === 300 && arr.every((v, i) => v === i), `len=${arr.length}`);

  const taken = yield* client.count({ n: 1_000_000 }).pipe(Stream.take(5), Stream.runCollect);
  check("early stream termination (client interrupt)", Array.from(taken).join() === "0,1,2,3,4");

  const big = yield* client.big();
  check("large frame split into parts and rejoined", big.length === 200_000 && big[0] === "é");

  const fiber = yield* Effect.forkChild(client.sleep());
  yield* Effect.sleep("50 millis");
  yield* Fiber.interrupt(fiber);
  check("interrupt of in-flight unary", true);

  const after = yield* client.echo({ a: 1 });
  check("connection healthy after interrupts", after.a === 1);
}).pipe(
  Effect.scoped,
  Effect.provide(
    RpcClient.layerProtocolSocket().pipe(
      Layer.provide(
        Layer.effect(Socket.Socket, Effect.map(Effect.service(Socket.Socket), joinSocketFrameParts)).pipe(
          Layer.provide(Socket.layerWebSocket(url.toString())),
        ),
      ),
      Layer.provide(Socket.layerWebSocketConstructorGlobal),
      Layer.provide(RpcSerialization.layerJson),
    ),
  ),
  Effect.timeout("20 seconds"),
);

const exit = await Effect.runPromiseExit(program);
for (const [name, ok, detail] of results) console.log(`${ok ? "PASS" : "FAIL"} ${name}${ok ? "" : ` — ${detail}`}`);
if (Exit.isFailure(exit)) console.log("FAIL program:", Cause.pretty(exit.cause));
const passed = Exit.isSuccess(exit) && results.every(([, ok]) => ok);
console.log(passed ? `ALL ${results.length} PASSED` : "CONFORMANCE FAILED");
process.exit(passed ? 0 : 1);

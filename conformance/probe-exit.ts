// Prints how Effect's JSON RPC codec encodes exits, to pin the Rust encoder.
import * as Schema from "effect/Schema";
import * as Exit from "effect/Exit";
import * as Rpc from "effect/rpc/Rpc";
class Nope extends Schema.TaggedError<Nope>()("Nope", { message: Schema.String }) {}
const streamRpc = Rpc.make("s", { success: Schema.Number, error: Nope, stream: true });
const unaryRpc = Rpc.make("u", { error: Nope });
const enc = (rpc: any, e: any) => JSON.stringify(Schema.encodeSync(Schema.toCodecJson(Rpc.exitSchema(rpc)))(e));
console.log("stream void ", enc(streamRpc, Exit.void));
console.log("unary void  ", enc(unaryRpc, Exit.void));
console.log("die str     ", enc(unaryRpc, Exit.die("boom")));
console.log("die error   ", enc(unaryRpc, Exit.die(new Error("boom"))));
console.log("fail        ", enc(unaryRpc, Exit.fail(new Nope({ message: "no" }))));
console.log("interrupt   ", enc(unaryRpc, Exit.interrupt()));
console.log("interrupt 3 ", enc(unaryRpc, Exit.interrupt(3)));

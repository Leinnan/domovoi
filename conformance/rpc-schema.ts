// Prints a compact TypeScript-like view of an RPC's payload, success (or
// stream element) and error schemas from t3code's contracts.
// Usage: node conformance/rpc-schema.ts <method>...
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import * as RpcSchema from "effect/rpc/RpcSchema";
import { WsRpcGroup } from "../.reference/t3code/packages/contracts/src/rpc.ts";

type Js = Record<string, any>;
const rpcs = (WsRpcGroup as unknown as { requests: ReadonlyMap<string, any> }).requests;

function render(s: Js, defs: Js, depth = 0, seen = new Set<string>()): string {
  if (!s || typeof s !== "object") return "unknown";
  if (s.$ref) {
    const name = String(s.$ref).split("/").pop()!;
    if (seen.has(name) || depth > 6) return name;
    const next = new Set(seen); next.add(name);
    return render(defs[name] ?? {}, defs, depth, next);
  }
  if (s.anyOf || s.oneOf) return (s.anyOf ?? s.oneOf).map((x: Js) => render(x, defs, depth + 1, seen)).join(" | ");
  if (s.allOf) return s.allOf.map((x: Js) => render(x, defs, depth + 1, seen)).join(" & ");
  if (s.const !== undefined) return JSON.stringify(s.const);
  if (s.enum) return s.enum.map((e: unknown) => JSON.stringify(e)).join(" | ");
  if (s.type === "array") return `Array<${render(s.items ?? {}, defs, depth + 1, seen)}>`;
  if (s.type === "object" || s.properties) {
    const req = new Set<string>(s.required ?? []);
    const props = Object.entries(s.properties ?? {}).map(([k, v]) => `${k}${req.has(k) ? "" : "?"}: ${render(v as Js, defs, depth + 1, seen)}`);
    if (s.additionalProperties && typeof s.additionalProperties === "object") props.push(`[k]: ${render(s.additionalProperties, defs, depth + 1, seen)}`);
    return `{ ${props.join("; ")} }`;
  }
  if (Array.isArray(s.type)) return s.type.join(" | ");
  return s.type ?? "unknown";
}

function show(label: string, schema: unknown) {
  const doc = Schema.toJsonSchemaDocument(schema as never) as Js;
  console.log(`  ${label}: ${render(doc.schema ?? doc, doc.definitions ?? doc.$defs ?? {})}`);
}

for (const tag of process.argv.slice(2)) {
  const rpc = rpcs.get(tag);
  if (!rpc) { console.log(`${tag}: unknown`); continue; }
  const stream = RpcSchema.getStreamSchemas(rpc.successSchema);
  console.log(`${tag}${Option.isSome(stream) ? " (stream)" : ""}`);
  show("payload", rpc.payloadSchema);
  show(Option.isSome(stream) ? "element" : "success", Option.isSome(stream) ? stream.value.success : rpc.successSchema);
  show("error", Option.isSome(stream) ? Schema.Union([rpc.errorSchema, stream.value.error]) : rpc.errorSchema);
}

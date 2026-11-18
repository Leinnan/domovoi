// Lists every method in t3code's WsRpcGroup: tag, stream or unary, and the
// permission t3code's server requires. Usage: node conformance/rpc-inventory.ts
import * as Option from "effect/Option";
import * as RpcSchema from "effect/rpc/RpcSchema";
import { WsRpcGroup } from "../.reference/t3code/packages/contracts/src/rpc.ts";
import { RPC_REQUIRED_SCOPES } from "../.reference/t3code/apps/server/src/auth/RpcAuthorization.ts";
import { clientRpcRequiredScopes } from "../.reference/t3code/packages/contracts/src/clientRpcPermissions.ts";

const rpcs = (WsRpcGroup as unknown as { requests: ReadonlyMap<string, { successSchema: unknown }> }).requests;
const rows = [...rpcs].map(([tag, rpc]) => ({
  tag,
  stream: Option.isSome(RpcSchema.getStreamSchemas(rpc.successSchema as never)),
  // Client-guarded scopes take precedence, as in `requiredScopesForRpcCall`.
  permission: clientRpcRequiredScopes(tag, undefined)[0] ?? (RPC_REQUIRED_SCOPES as Record<string, string>)[tag] ?? "(payload-dependent)",
}));
console.log(JSON.stringify(rows));

// Prints t3code's required permission for each given RPC method, from the
// server's own table (apps/server/src/auth/RpcAuthorization.ts).
// Usage: node conformance/rpc-scopes.ts <method>...
import { requiredScopeForRpcMethod } from "../.reference/t3code/apps/server/src/auth/RpcAuthorization.ts";
import { requiredScopesForProjectMutation } from "../.reference/t3code/packages/contracts/src/index.ts";

const out: Record<string, unknown> = {};
for (const m of process.argv.slice(2)) out[m] = requiredScopeForRpcMethod(m);
out["projects.mutate:project.create"] = requiredScopesForProjectMutation({ type: "project.create" } as never);
out["projects.mutate:project.delete"] = requiredScopesForProjectMutation({ type: "project.delete" } as never);
console.log(JSON.stringify(out, null, 1));

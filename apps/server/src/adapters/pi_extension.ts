// Domovoi's pi extension, passed to every `pi --mode rpc` Domovoi runs for a
// thread (`--extension`). Ported from t3code `provider-pi` (MIT,
// packages/provider-pi/src/server/mcpExtensionSource.ts).
//
// - Approvals: pi has no permission prompts of its own and leaves policy to
//   extensions. The `tool_call` hook asks through `ctx.ui.confirm`, which pi
//   sends Domovoi as an `extension_ui_request`, following the thread's runtime
//   mode (DOMOVOI_PI_RUNTIME_MODE; one mode per process, Domovoi restarts pi
//   when it changes).
// - Domovoi's agent MCP tools over Streamable HTTP (DOMOVOI_MCP_URL and
//   DOMOVOI_MCP_TOKEN), registered as `mcp__<server>__<tool>`.
// - `$skill` chips, a service-tier command, and a notice for an untrusted folder.
//
// `__DOMOVOI_MCP_SERVER__` is replaced with the MCP server's name when the
// file is written. Imports resolve from the user's pi install (pi aliases them).
import { stripFrontmatter, type ExtensionAPI, type ExtensionContext } from "@earendil-works/pi-coding-agent";
import * as NodeCrypto from "node:crypto";
import * as NodeFSP from "node:fs/promises";
import * as NodePath from "node:path";
import { Type } from "typebox";

const URL_ENV = "DOMOVOI_MCP_URL";
const TOKEN_ENV = "DOMOVOI_MCP_TOKEN";
const INSTRUCTIONS_ENV = "DOMOVOI_MCP_INSTRUCTIONS";
const RUNTIME_MODE_ENV = "DOMOVOI_PI_RUNTIME_MODE";
const EXTENSION_PATH_ENV = "DOMOVOI_PI_EXTENSION_PATH";
const SERVER = "__DOMOVOI_MCP_SERVER__";
const SERVICE_TIER_COMMAND = "domovoi-service-tier";
const PROTOCOL = "2025-06-18";
const READ_ONLY_TOOLS = new Set(["read", "grep", "find", "ls"]);
// These ask as file changes, and Auto-accept edits allows them.
const FILE_CHANGE_TOOLS = new Set(["edit", "write"]);
// Offered to the model up front; the other Domovoi tools are found with tool_search.
const DIRECT_TOOLS = new Set(["delegate_task", "task_status"]);

type RuntimeMode = "approval-required" | "auto-accept-edits" | "auto" | "full-access";

type JsonValue = string | number | boolean | null | JsonValue[] | { [key: string]: JsonValue };

type JsonRpcResponse = {
  readonly id?: number | string;
  readonly result?: JsonValue;
  readonly error?: { readonly message?: string };
};

type McpTool = {
  readonly name: string;
  readonly description?: string;
  readonly inputSchema?: Record<string, unknown>;
  readonly annotations?: { readonly readOnlyHint?: boolean };
  readonly outputSchema?: Record<string, unknown>;
};

function env(name: string): string | undefined {
  const value = process.env[name];
  return value && value.length > 0 ? value : undefined;
}

function runtimeMode(): RuntimeMode {
  const value = env(RUNTIME_MODE_ENV);
  return value === "approval-required" || value === "auto-accept-edits" || value === "auto" || value === "full-access"
    ? value
    : "full-access";
}

// Domovoi keys "accept for session" on this text, so a cut summary ends with
// the full input's hash: calls that differ only past the cut ask again.
function toolInputSummary(input: unknown): string {
  let text: string;
  try {
    text = JSON.stringify(input, null, 2) ?? String(input);
  } catch {
    text = String(input);
  }
  if (text.length <= 4_000) return text;
  const hash = NodeCrypto.createHash("sha256").update(text, "utf16le").digest("hex");
  return text.slice(0, 4_000) + "\n... " + (text.length - 4_000) + " more characters not shown (sha256 " + hash + ")";
}

function parseSseOrJson(body: string, contentType: string): JsonRpcResponse {
  if (contentType.includes("text/event-stream")) {
    for (const line of body.split("\n")) {
      const trimmed = line.startsWith("data:") ? line.slice(5).trim() : "";
      if (trimmed.length === 0) continue;
      const parsed = JSON.parse(trimmed) as JsonRpcResponse;
      if (parsed.id !== undefined || parsed.result !== undefined || parsed.error !== undefined) return parsed;
    }
    throw new Error("MCP SSE response had no JSON-RPC payload.");
  }
  return JSON.parse(body) as JsonRpcResponse;
}

function jsonSchemaToTypebox(schema: Record<string, unknown> | undefined) {
  const unsafe = (Type as { Unsafe?: (value: unknown) => unknown }).Unsafe;
  if (typeof unsafe === "function" && schema !== undefined) {
    // Zero-argument MCP tools may omit "properties", which some
    // OpenAI-compatible servers reject: declare it, as pi does for its own MCP tools.
    return unsafe({ ...schema, type: schema.type ?? "object", ...(schema.properties === undefined ? { properties: {} } : {}) });
  }
  return Type.Object({}, { additionalProperties: true });
}

type ModelContent = { type: "text"; text: string } | { type: "image"; data: string; mimeType: string };

function mcpModelContent(result: unknown): ModelContent[] {
  if (result === null || result === undefined) return [];
  if (typeof result !== "object") return [{ type: "text", text: String(result) }];
  const record = result as {
    readonly content?: ReadonlyArray<{ readonly type?: string; readonly text?: string; readonly data?: string; readonly mimeType?: string }>;
    readonly structuredContent?: unknown;
  };
  const content: ModelContent[] = [];
  if (Array.isArray(record.content)) {
    for (const part of record.content) {
      if (part?.type === "text" && typeof part.text === "string") {
        content.push({ type: "text", text: part.text });
      } else if (part?.type === "image" && typeof part.data === "string" && typeof part.mimeType === "string") {
        content.push({ type: "image", data: part.data, mimeType: part.mimeType });
      }
    }
  }
  // Structured output is for scripts: model text only when the server sent no
  // model-facing blocks, as pi's own MCP tools do.
  if (content.length === 0) content.push({ type: "text", text: JSON.stringify(record.structuredContent ?? result) });
  return content;
}

function mcpScriptResult(result: JsonValue | undefined) {
  if (typeof result !== "object" || result === null || Array.isArray(result)) return result;
  const { _meta: _ignored, ...scriptResult } = result;
  return scriptResult;
}

function mcpOutputSchema(tool: McpTool) {
  return jsonSchemaToTypebox({
    type: "object",
    properties: {
      content: { type: "array", items: { type: "object" } },
      ...(tool.outputSchema === undefined ? {} : { structuredContent: tool.outputSchema }),
      isError: { type: "boolean" },
      _meta: { type: "object" },
    },
    required: ["content"],
  });
}

function isMcpToolError(result: unknown): boolean {
  return typeof result === "object" && result !== null && "isError" in result && result.isError === true;
}

function createMcpClient(endpoint: string, token: string) {
  let nextId = 1;
  let sessionId: string | undefined;
  const headers = (): Record<string, string> => {
    const next: Record<string, string> = {
      accept: "application/json, text/event-stream",
      authorization: token.startsWith("Bearer ") ? token : `Bearer ${token}`,
      "content-type": "application/json",
      "mcp-protocol-version": PROTOCOL,
    };
    if (sessionId !== undefined) next["mcp-session-id"] = sessionId;
    return next;
  };
  const request = async (method: string, params?: unknown, signal?: AbortSignal) => {
    const id = nextId++;
    const response = await fetch(endpoint, { method: "POST", headers: headers(), body: JSON.stringify({ jsonrpc: "2.0", id, method, params }), signal });
    const nextSession = response.headers.get("mcp-session-id");
    if (nextSession) sessionId = nextSession;
    const body = await response.text();
    if (!response.ok) throw new Error(`MCP ${method} failed (${response.status}): ${body.slice(0, 400)}`);
    if (body.length === 0) return undefined;
    const parsed = parseSseOrJson(body, response.headers.get("content-type") ?? "");
    if (parsed.error) throw new Error(parsed.error.message ?? `MCP ${method} returned an error`);
    return parsed.result;
  };
  const notify = async (method: string, params?: unknown, signal?: AbortSignal) => {
    await fetch(endpoint, { method: "POST", headers: headers(), body: JSON.stringify({ jsonrpc: "2.0", method, params }), signal });
  };
  return {
    async connect(signal?: AbortSignal) {
      await request("initialize", { protocolVersion: PROTOCOL, capabilities: {}, clientInfo: { name: "domovoi-pi-mcp", version: "1.0.0" } }, signal);
      await notify("notifications/initialized", {}, signal).catch(() => undefined);
    },
    async listTools(signal?: AbortSignal) {
      const tools: McpTool[] = [];
      let cursor: string | undefined;
      do {
        const result = (await request("tools/list", cursor === undefined ? {} : { cursor }, signal)) as { tools?: McpTool[]; nextCursor?: string } | undefined;
        tools.push(...(result?.tools ?? []));
        cursor = result?.nextCursor;
      } while (cursor);
      return tools;
    },
    async callTool(name: string, args: Record<string, unknown>, signal?: AbortSignal) {
      return request("tools/call", { name, arguments: args }, signal);
    },
  };
}

export default async function domovoiExtension(pi: ExtensionAPI) {
  // RPC has no trust prompt, so pi skips an untrusted folder's own resources.
  // Session replacement reruns this factory, so check each session's first
  // turn. An explicit --no-approve is deliberate and stays quiet.
  let projectTrustChecked = false;
  pi.on("before_agent_start", (_event, ctx) => {
    if (projectTrustChecked) return;
    projectTrustChecked = true;
    if (ctx.isProjectTrusted() || process.argv.some((arg) => arg === "--no-approve" || arg === "-na")) return;
    ctx.ui.notify(
      `Pi did not load project settings, extensions, skills, prompts, MCP servers or system prompt files for ${ctx.cwd} because the folder is not trusted. To load them, run pi there (with the same PI_CODING_AGENT_DIR, if set), trust the folder or its parent with /trust, then start a new thread in that folder. To trust every project, start the Domovoi server with DOMOVOI_PI_ARGS=--approve; to keep them off without this notice, use --no-approve.`,
      "warning",
    );
  });

  // Skill chips ($name) stay in the request: pi expands only one leading
  // /skill: command, so load every chosen skill here with pi's own loader.
  pi.on("input", async (event, ctx) => {
    if (!event.text.includes("$")) return;
    const commands = pi.getCommands().filter((command) => command.source === "skill");
    const nativeSkill = /^\/skill:([^ ]+)(?: |$)/.exec(event.text)?.[1];
    const seen = new Set<string>(nativeSkill === undefined ? [] : [nativeSkill]);
    const blocks: string[] = [];
    for (const match of event.text.matchAll(/(^|\s)\$([^\s]+)(?=\s|$)/g)) {
      const name = match[2];
      if (name === undefined || seen.has(name)) continue;
      const command = commands.find((candidate) => candidate.name === "skill:" + name);
      if (command === undefined) continue;
      seen.add(name);
      const path = command.sourceInfo.path;
      try {
        const body = stripFrontmatter(await NodeFSP.readFile(path, "utf8")).trim();
        blocks.push(`<skill name="${name}" location="${path}">\nReferences are relative to ${NodePath.dirname(path)}.\n\n${body}\n</skill>`);
      } catch {
        ctx.ui.notify(`Could not load skill ${name} from ${path}.`, "error");
      }
    }
    if (blocks.length === 0) return;
    return { action: "transform", text: event.text + "\n\n" + blocks.join("\n\n"), images: event.images };
  });

  // pi has no service tier setting: Domovoi sets one with this command before
  // each turn. Only a tier the active model advertises is sent; Standard sends none.
  let serviceTier: string | undefined;
  pi.registerCommand(SERVICE_TIER_COMMAND, {
    description: "Set the service tier Domovoi sends with provider requests",
    handler: async (args) => {
      serviceTier = args.trim() || undefined;
    },
  });

  pi.on("before_provider_request", (event, ctx) => {
    const payload = event.payload;
    if (typeof payload !== "object" || payload === null || Array.isArray(payload)) return;
    const replacement = { ...payload } as Record<string, unknown>;
    let changed = false;
    // pi reuses the previous response's usage when a fork's prompt differs and
    // reserves nearly all the rest of the context for output, which OpenRouter
    // refuses even for a short conversation. Cap it until pi budgets reliably.
    if (ctx.model?.provider === "openrouter") {
      for (const key of ["max_tokens", "max_completion_tokens"]) {
        const limit = replacement[key];
        if (typeof limit === "number" && Number.isFinite(limit) && limit > 32_768) {
          replacement[key] = 32_768;
          changed = true;
        }
      }
    }
    const tiers = (ctx.model as { serviceTiers?: unknown } | undefined)?.serviceTiers;
    if (serviceTier !== undefined && Array.isArray(tiers) && tiers.some((tier) => tier?.id === serviceTier)) {
      replacement.service_tier = serviceTier;
      changed = true;
    }
    if (changed) return replacement;
  });

  // Only pi's own search is known to be read-only. An extension that replaces
  // it keeps the name, and cannot find deferred tools either.
  const hasBuiltinToolSearch = () =>
    typeof pi.getAllTools === "function" &&
    pi.getAllTools().some((tool) => tool.name === "tool_search" && tool.sourceInfo?.path === "builtin:tool-search");

  const readOnlyMcpTools = new Set<string>();
  const isReadOnlyMcpTool = (name: string) => {
    const extensionPath = env(EXTENSION_PATH_ENV);
    if (extensionPath === undefined || !readOnlyMcpTools.has(name) || typeof pi.getAllTools !== "function") return false;
    // A user extension may own the same name: only this bridge's tools carry
    // the Domovoi server's read-only annotation.
    return pi.getAllTools().some(
      (tool) => tool.name === name && typeof tool.sourceInfo?.path === "string" && NodePath.resolve(tool.sourceInfo.path) === NodePath.resolve(extensionPath),
    );
  };

  pi.on("tool_call", async (event, ctx) => {
    const mode = runtimeMode();
    if (mode === "full-access") return;
    if ((event.toolName === "tool_search" ? hasBuiltinToolSearch() : READ_ONLY_TOOLS.has(event.toolName)) || isReadOnlyMcpTool(event.toolName)) return;
    if (mode === "auto-accept-edits" && FILE_CHANGE_TOOLS.has(event.toolName)) return;
    const approved = await ctx.ui.confirm(`Allow ${event.toolName}?`, toolInputSummary(event.input), { signal: ctx.signal });
    if (!approved) return { block: true, reason: `${event.toolName} was declined in Domovoi.` };
  });

  const endpoint = env(URL_ENV);
  const token = env(TOKEN_ENV);
  // Agent tools are off (DOMOVOI_NO_AGENT_TOOLS) or not served: approvals still apply.
  if (endpoint === undefined || token === undefined) return;

  // Tool exposure arrived with registerMcpServer in pi 0.99. This bridge keeps
  // the credentials: an mcp.json entry for the same server must not replace it.
  const supportsExposure = "registerMcpServer" in pi && typeof pi.registerMcpServer === "function";
  let deferOptionalTools = supportsExposure;
  let catalog: ReadonlyArray<McpTool> = [];
  const client = createMcpClient(endpoint, token);
  let started: Promise<void> | undefined;
  const publicPrefix = `mcp__${SERVER}__`;
  // A hidden canonical spelling reserves the name against pi's own MCP servers.
  const hiddenPrefix = `mcp__${SERVER.replace(/-/g, "_")}__`;

  const registerTools = () => {
    const prefixes = supportsExposure && hiddenPrefix !== publicPrefix ? [publicPrefix, hiddenPrefix] : [publicPrefix];
    readOnlyMcpTools.clear();
    for (const tool of catalog) {
      const name = tool.name;
      for (const prefix of prefixes) {
        if (tool.annotations?.readOnlyHint === true) readOnlyMcpTools.add(`${prefix}${name}`);
        const exposure = prefix === hiddenPrefix && prefix !== publicPrefix ? "hidden" : deferOptionalTools && !DIRECT_TOOLS.has(name) ? "deferred" : "direct";
        pi.registerTool({
          name: `${prefix}${name}`,
          label: name,
          description: tool.description ?? name,
          parameters: jsonSchemaToTypebox(tool.inputSchema),
          ...(supportsExposure ? { outputSchema: mcpOutputSchema(tool) } : {}),
          ...(supportsExposure ? { exposure } : {}),
          async execute(_toolCallId, params, signal) {
            const result = await client.callTool(name, (params ?? {}) as Record<string, unknown>, signal);
            const content = mcpModelContent(result);
            if (isMcpToolError(result) && !content.some((part) => part.type === "text" && part.text.length > 0)) {
              content.push({ type: "text", text: `MCP tool ${SERVER}/${name} returned an error` });
            }
            return {
              content,
              structuredContent: mcpScriptResult(result),
              details: { server: SERVER, tool: name },
              ...(isMcpToolError(result) ? { isError: true } : {}),
            };
          },
        });
      }
    }
  };

  const ensureStarted = () => {
    if (started !== undefined) return started;
    const attempt = (async () => {
      const signal = AbortSignal.timeout(10_000);
      await client.connect(signal);
      catalog = await client.listTools(signal);
      registerTools();
    })();
    started = attempt;
    void attempt.catch(() => {
      if (started === attempt) started = undefined;
    });
    return attempt;
  };

  // CLI extensions load before builtins, and pi keeps a tool name's first
  // registration: register now, and retry a failed connection at session_start.
  await ensureStarted().catch(() => undefined);

  const reconcileDiscovery = (ctx: Pick<ExtensionContext, "model">) => {
    if (!supportsExposure) return;
    const hasToolSearch = hasBuiltinToolSearch();
    const exposureChanged = deferOptionalTools !== hasToolSearch;
    deferOptionalTools = hasToolSearch;
    if (exposureChanged) registerTools();
    const active = pi.getActiveTools();
    // Completion gateways can drop calls to undeclared tools named in history:
    // load Domovoi's schemas for them (pi still applies --tools/--exclude-tools).
    const required = [
      ...(hasToolSearch ? ["tool_search"] : []),
      ...(ctx.model?.api === "openai-completions" ? catalog.map((tool) => publicPrefix + tool.name) : []),
    ];
    const missing = required.filter((name) => !active.includes(name));
    if (missing.length > 0) pi.setActiveTools([...active, ...missing]);
  };

  // Tree navigation restores its saved loadout after session_start.
  pi.on("session_tree", (_event, ctx) => reconcileDiscovery(ctx));
  pi.on("model_select", (_event, ctx) => reconcileDiscovery(ctx));
  // What Domovoi's tools are for, in the system prompt (so leading slash commands still expand).
  pi.on("before_agent_start", (event, ctx) => {
    reconcileDiscovery(ctx);
    const instructions = env(INSTRUCTIONS_ENV);
    if (instructions === undefined || instructions === "") return undefined;
    const sections = event.systemPromptOptions?.sections;
    if (sections && typeof sections === "object" && event.systemPromptOptions?.forceSystemPrompt === undefined) {
      sections.domovoi_orchestration = instructions;
      return undefined;
    }
    return typeof event.systemPrompt === "string" ? { systemPrompt: event.systemPrompt + "\n\n" + instructions } : undefined;
  });
  pi.on("session_start", async (_event, ctx) => {
    reconcileDiscovery(ctx);
    try {
      await ensureStarted();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      ctx.ui.notify(`Domovoi's agent tools are unavailable: ${message}`, "warning");
    }
  });
}

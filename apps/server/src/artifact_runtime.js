// Domovoi offline React / JSX / TSX artifact runtime.
// Runs inside an isolated sandboxed WebView with zero external network access.
// Provides:
//  1. A React-API-compatible vDOM + hooks runtime (useState, useEffect, useMemo,
//     useCallback, useRef, useReducer, createContext, useContext, Fragment, memo).
//  2. Lightweight TypeScript type stripping + JSX transpiler supporting fragments,
//     nested elements, expressions, spread props, and ES imports/exports.
//  3. Common Lucide icon SVG helpers + responsive utility styling.
//  4. Read-only error reporting back to Domovoi native shells ("Fix with Claude").
(() => {
  "use strict";

  function reportError(message, line) {
    const payload = { kind: "error", message: String(message || "Runtime error"), line: Number(line) || 0 };
    try {
      if (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.domovoiArtifact) {
        window.webkit.messageHandlers.domovoiArtifact.postMessage(payload);
      } else if (window.DomovoiArtifact && typeof window.DomovoiArtifact.onError === "function") {
        window.DomovoiArtifact.onError(payload.message, payload.line);
      }
    } catch (_) {}
  }

  window.addEventListener("error", (ev) => {
    reportError(ev.message || (ev.error && ev.error.message) || "Script error", ev.lineno || 0);
  });
  window.addEventListener("unhandledrejection", (ev) => {
    const r = ev.reason;
    reportError((r && (r.message || String(r))) || "Unhandled promise rejection", 0);
  });

  // ---- Minimal React-compatible vDOM & hooks engine ----
  const FRAGMENT = Symbol.for("domovoi.fragment");
  const SVG_NS = "http://www.w3.org/2000/svg";
  const SVG_TAGS = new Set([
    "svg", "path", "circle", "rect", "line", "polyline", "polygon", "ellipse",
    "g", "defs", "linearGradient", "radialGradient", "stop", "text", "tspan",
    "clipPath", "mask", "use", "symbol", "marker", "pattern", "filter", "feGaussianBlur",
  ]);

  function flatten(arr, out) {
    for (let i = 0; i < arr.length; i++) {
      const c = arr[i];
      if (Array.isArray(c)) flatten(c, out);
      else if (c !== null && c !== undefined && typeof c !== "boolean") out.push(c);
    }
    return out;
  }

  function createElement(type, props, ...children) {
    const p = props ? Object.assign({}, props) : {};
    const flat = flatten(children, []);
    if (flat.length === 1) p.children = flat[0];
    else if (flat.length > 1) p.children = flat;
    return { type, props: p, key: p.key };
  }

  function cloneElement(el, props, ...children) {
    if (!el || typeof el !== "object") return el;
    const merged = Object.assign({}, el.props, props || {});
    if (children.length > 0) {
      const flat = flatten(children, []);
      merged.children = flat.length === 1 ? flat[0] : flat;
    }
    return { type: el.type, props: merged, key: merged.key };
  }

  let currentRoot = null;
  let hookPath = "";
  let hookIdx = 0;
  const hookStore = new Map(); // path -> array of hook slots
  const pendingEffects = [];
  const activeContexts = new Map();

  function getHooks(path) {
    let arr = hookStore.get(path);
    if (!arr) { arr = []; hookStore.set(path, arr); }
    return arr;
  }

  let scheduled = false;
  function scheduleRender() {
    if (scheduled || !currentRoot) return;
    scheduled = true;
    queueMicrotask(() => {
      scheduled = false;
      if (currentRoot) currentRoot.render();
    });
  }

  function useState(initial) {
    const hooks = getHooks(hookPath);
    const i = hookIdx++;
    if (hooks.length <= i) {
      hooks.push(typeof initial === "function" ? initial() : initial);
    }
    const setState = (next) => {
      const prev = hooks[i];
      const val = typeof next === "function" ? next(prev) : next;
      if (!Object.is(prev, val)) {
        hooks[i] = val;
        scheduleRender();
      }
    };
    return [hooks[i], setState];
  }

  function useReducer(reducer, initialArg, init) {
    const [state, setState] = useState(() => (init ? init(initialArg) : initialArg));
    const dispatch = (action) => setState((prev) => reducer(prev, action));
    return [state, dispatch];
  }

  function useRef(initial) {
    const hooks = getHooks(hookPath);
    const i = hookIdx++;
    if (hooks.length <= i) hooks.push({ current: initial });
    return hooks[i];
  }

  function depsChanged(a, b) {
    if (!a || !b || a.length !== b.length) return true;
    for (let i = 0; i < a.length; i++) if (!Object.is(a[i], b[i])) return true;
    return false;
  }

  function useMemo(factory, deps) {
    const hooks = getHooks(hookPath);
    const i = hookIdx++;
    if (hooks.length <= i || depsChanged(hooks[i].deps, deps)) {
      hooks[i] = { value: factory(), deps };
    }
    return hooks[i].value;
  }

  function useCallback(fn, deps) {
    return useMemo(() => fn, deps);
  }

  function useEffect(effect, deps) {
    const hooks = getHooks(hookPath);
    const i = hookIdx++;
    const prev = hooks[i];
    if (!prev || deps === undefined || depsChanged(prev.deps, deps)) {
      const slot = { deps, cleanup: prev ? prev.cleanup : null };
      hooks[i] = slot;
      pendingEffects.push(() => {
        if (typeof slot.cleanup === "function") {
          try { slot.cleanup(); } catch (e) { reportError(e.message || e); }
        }
        try { slot.cleanup = effect(); } catch (e) { reportError(e.message || e); }
      });
    }
  }

  function createContext(defaultValue) {
    const ctx = {
      _default: defaultValue,
      Provider: function Provider(props) {
        const prev = activeContexts.has(ctx) ? activeContexts.get(ctx) : undefined;
        activeContexts.set(ctx, props.value);
        try {
          return createElement(FRAGMENT, null, props.children);
        } finally {
          if (prev === undefined) activeContexts.delete(ctx);
          else activeContexts.set(ctx, prev);
        }
      },
    };
    return ctx;
  }

  function useContext(ctx) {
    return activeContexts.has(ctx) ? activeContexts.get(ctx) : ctx._default;
  }

  function applyStyle(el, style) {
    if (typeof style === "string") { el.setAttribute("style", style); return; }
    if (!style || typeof style !== "object") return;
    const unitless = new Set(["opacity", "zIndex", "fontWeight", "lineHeight", "flex", "flexGrow", "flexShrink", "order", "zoom", "gridColumn", "gridRow"]);
    for (const [k, v] of Object.entries(style)) {
      if (v === null || v === undefined) continue;
      if (k.startsWith("--")) el.style.setProperty(k, String(v));
      else el.style[k] = typeof v === "number" && !unitless.has(k) ? v + "px" : String(v);
    }
  }

  function buildDom(vnode, path, isSvg) {
    if (vnode === null || vnode === undefined || typeof vnode === "boolean") {
      return document.createTextNode("");
    }
    if (typeof vnode === "string" || typeof vnode === "number") {
      return document.createTextNode(String(vnode));
    }
    if (Array.isArray(vnode)) {
      const frag = document.createDocumentFragment();
      for (let i = 0; i < vnode.length; i++) {
        frag.appendChild(buildDom(vnode[i], path + "." + i, isSvg));
      }
      return frag;
    }
    if (vnode.type === FRAGMENT) {
      const frag = document.createDocumentFragment();
      const ch = vnode.props && vnode.props.children;
      if (ch !== undefined) frag.appendChild(buildDom(ch, path + ".f", isSvg));
      return frag;
    }
    if (typeof vnode.type === "function") {
      const prevPath = hookPath;
      const prevIdx = hookIdx;
      hookPath = path + ":" + (vnode.key !== undefined ? String(vnode.key) : (vnode.type.name || "C"));
      hookIdx = 0;
      let rendered;
      try {
        rendered = vnode.type(vnode.props || {});
      } catch (e) {
        reportError(e && e.message ? e.message : String(e));
        const errBox = document.createElement("div");
        errBox.className = "domovoi-runtime-error";
        errBox.textContent = "Component error: " + (e && e.message ? e.message : String(e));
        hookPath = prevPath;
        hookIdx = prevIdx;
        return errBox;
      }
      hookPath = prevPath;
      hookIdx = prevIdx;
      return buildDom(rendered, path + ".c", isSvg);
    }

    const tag = String(vnode.type);
    const svg = isSvg || SVG_TAGS.has(tag);
    const el = svg ? document.createElementNS(SVG_NS, tag) : document.createElement(tag);
    const props = vnode.props || {};
    for (const [k, v] of Object.entries(props)) {
      if (k === "children" || k === "key" || v === undefined || v === null) continue;
      if (k === "ref") {
        if (typeof v === "function") v(el);
        else if (v && typeof v === "object") v.current = el;
      } else if (k === "dangerouslySetInnerHTML" && v && typeof v.__html === "string") {
        el.innerHTML = v.__html;
      } else if (k === "style") {
        applyStyle(el, v);
      } else if (k === "className" || k === "class") {
        if (svg) el.setAttribute("class", String(v));
        else el.className = String(v);
      } else if (k === "htmlFor") {
        el.setAttribute("for", String(v));
      } else if (k.startsWith("on") && typeof v === "function") {
        const evName = k.slice(2).toLowerCase();
        const mapped = evName === "change" && (tag === "input" || tag === "textarea") && props.type !== "checkbox" && props.type !== "radio" ? "input" : evName;
        el.addEventListener(mapped, v);
      } else if (k === "value" && (tag === "input" || tag === "textarea" || tag === "select")) {
        el.value = String(v);
        el.setAttribute("value", String(v));
      } else if (k === "checked" && tag === "input") {
        el.checked = Boolean(v);
      } else if (typeof v === "boolean") {
        if (v) el.setAttribute(k, "");
      } else {
        el.setAttribute(k, String(v));
      }
    }
    if (props.children !== undefined && !props.dangerouslySetInnerHTML) {
      el.appendChild(buildDom(props.children, path + ".0", svg));
    }
    return el;
  }

  const React = {
    Fragment: FRAGMENT,
    createElement,
    cloneElement,
    useState,
    useEffect,
    useLayoutEffect: useEffect,
    useMemo,
    useCallback,
    useRef,
    useReducer,
    createContext,
    useContext,
    memo: (c) => c,
    forwardRef: (fn) => (props) => fn(props, props.ref),
    StrictMode: FRAGMENT,
  };

  function createRoot(container) {
    let rootElement = null;
    const root = {
      render(vnode) {
        if (vnode !== undefined) rootElement = vnode;
        currentRoot = root;
        const activeEl = document.activeElement;
        const focusSel = activeEl && (activeEl.tagName === "INPUT" || activeEl.tagName === "TEXTAREA")
          ? { id: activeEl.id, name: activeEl.name, start: activeEl.selectionStart, end: activeEl.selectionEnd }
          : null;
        pendingEffects.length = 0;
        const dom = buildDom(rootElement, "r", false);
        container.innerHTML = "";
        container.appendChild(dom);
        if (focusSel && (focusSel.id || focusSel.name)) {
          const restored = focusSel.id
            ? document.getElementById(focusSel.id)
            : container.querySelector(`[name="${CSS.escape(focusSel.name)}"]`);
          if (restored && typeof restored.focus === "function") {
            restored.focus();
            if (focusSel.start != null && restored.setSelectionRange) {
              try { restored.setSelectionRange(focusSel.start, focusSel.end); } catch (_) {}
            }
          }
        }
        const fx = pendingEffects.slice();
        pendingEffects.length = 0;
        for (const fn of fx) fn();
      },
    };
    return root;
  }

  // ---- Built-in Lucide icon proxy ----
  const LUCIDE_PATHS = {
    Check: "M20 6 9 17l-5-5",
    X: "M18 6 6 18M6 6l12 12",
    Plus: "M5 12h14M12 5v14",
    Minus: "M5 12h14",
    Search: "M21 21l-4.35-4.35M11 19a8 8 0 1 0 0-16 8 8 0 0 0 0 16Z",
    ChevronRight: "m9 18 6-6-6-6",
    ChevronDown: "m6 9 6 6 6-6",
    ChevronUp: "m18 15-6-6-6 6",
    ChevronLeft: "m15 18-6-6 6-6",
    ArrowRight: "M5 12h14M12 5l7 7-7 7",
    ArrowLeft: "M19 12H5M12 19l-7-7 7-7",
    ArrowUp: "M12 19V5M5 12l7-7 7 7",
    ArrowDown: "M12 5v14M19 12l-7 7-7-7",
    RefreshCw: "M3 12a9 9 0 0 1 15-6.7L21 8M21 3v5h-5M21 12a9 9 0 0 1-15 6.7L3 16M3 21v-5h5",
    Play: "M6 4l14 8-14 8V4Z",
    Pause: "M6 4h4v16H6zM14 4h4v16h-4z",
    Trash2: "M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6M10 11v6M14 11v6",
    Edit: "M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7M18.5 2.5a2.12 2.12 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5Z",
    Copy: "M8 4H6a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h8a2 2 0 0 0 2-2v-2M10 4h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-8a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2Z",
    Sun: "M12 16a4 4 0 1 0 0-8 4 4 0 0 0 0 8ZM12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41",
    Moon: "M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79Z",
    Settings: "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z",
    AlertCircle: "M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20ZM12 8v4M12 16h.01",
    Info: "M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20ZM12 16v-4M12 8h.01",
    Star: "m12 2 3.09 6.26L22 9.27l-5 4.87 1.18 6.88L12 17.77l-6.18 3.25L7 14.14 2 9.27l6.91-1.01L12 2Z",
    Sparkles: "m12 3 1.5 4.5L18 9l-4.5 1.5L12 15l-1.5-4.5L6 9l4.5-1.5L12 3Z",
    Download: "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3",
    Filter: "M22 3H2l8 9.46V19l4 2v-8.54L22 3Z",
    TrendingUp: "m22 7-8.5 8.5-5-5L2 17M16 7h6v6",
    BarChart2: "M18 20V10M12 20V4M6 20v-6",
    Activity: "M22 12h-4l-3 9L9 3l-3 9H2",
    Clock: "M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20ZM12 6v6l4 2",
    Calendar: "M8 2v4M16 2v4M3 10h18M5 4h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2Z",
    User: "M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2M12 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8Z",
    Folder: "M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2Z",
    FileText: "M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8ZM14 2v6h6M16 13H8M16 17H8M10 9H8",
    Code: "m16 18 6-6-6-6M8 6l-6 6 6 6",
    Terminal: "m4 17 6-6-6-6M12 19h8",
    ExternalLink: "M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6M15 3h6v6M10 14 21 3",
  };

  const lucideProxy = new Proxy({}, {
    get(_t, name) {
      if (typeof name !== "string") return undefined;
      return function LucideIcon(props) {
        const size = (props && props.size) || 18;
        const d = LUCIDE_PATHS[name] || "M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20Z";
        return createElement("svg", {
          width: size, height: size, viewBox: "0 0 24 24", fill: "none",
          stroke: (props && props.color) || "currentColor", strokeWidth: (props && props.strokeWidth) || 2,
          strokeLinecap: "round", strokeLinejoin: "round",
          className: props && props.className, style: props && props.style,
        }, createElement("path", { d }));
      };
    },
  });

  // ---- Lightweight TypeScript + JSX Transpiler ----
  function stripTypesAndImports(src) {
    let code = src.replace(/\r\n/g, "\n");
    // Collect imports from react / lucide-react into local bindings.
    const preamble = [];
    code = code.replace(/^\s*import\s+([\s\S]*?)\s+from\s+["']([^"']+)["'];?\s*$/gm, (_, spec, mod) => {
      if (mod === "react" || mod === "react-dom" || mod === "react-dom/client") {
        const m = spec.match(/\{([\s\S]*?)\}/);
        if (m) {
          const names = m[1].split(",").map((s) => s.trim()).filter(Boolean).map((s) => s.replace(/\s+as\s+/, ": "));
          preamble.push(`const { ${names.join(", ")} } = React;`);
        }
        return "";
      }
      if (mod === "lucide-react") {
        const m = spec.match(/\{([\s\S]*?)\}/);
        if (m) {
          const names = m[1].split(",").map((s) => s.trim()).filter(Boolean).map((s) => s.replace(/\s+as\s+/, ": "));
          preamble.push(`const { ${names.join(", ")} } = __lucide;`);
        }
        return "";
      }
      return "";
    });
    // Strip TypeScript interfaces (both single-line and multi-line) with balanced brace matching.
    code = code.replace(/^\s*(?:export\s+)?interface\s+\w+(?:<[^>]*>)?(?:\s+extends\s+[^{]+)?\s*\{/gm, (match, offset, full) => {
      return "__DOMOVOI_STRIP_IFACE__";
    });
    while (code.includes("__DOMOVOI_STRIP_IFACE__")) {
      const idx = code.indexOf("__DOMOVOI_STRIP_IFACE__");
      let j = idx + "__DOMOVOI_STRIP_IFACE__".length;
      let depth = 1;
      while (j < code.length && depth > 0) {
        if (code[j] === "{") depth++;
        else if (code[j] === "}") depth--;
        j++;
      }
      code = code.slice(0, idx) + code.slice(j);
    }
    code = code.replace(/^\s*(?:export\s+)?type\s+\w+(?:<[^>]*>)?\s*=\s*[^;]+;/gm, "");
    // Strip generic type arguments on common React hooks: useState<Foo>(...) -> useState(...)
    code = code.replace(/\b(useState|useRef|useReducer|useMemo|useCallback|createContext)<[^()>\n]+>\(/g, "$1(");
    // Strip destructured prop type annotations: `({ a, b }: Props)` -> `({ a, b })`
    code = code.replace(/\}\s*:\s*[A-Z]\w*(?:<[^)]*>)?(?:\s*\|\s*[A-Za-z0-9_<>\[\]]+)*\s*(?=[,)])/g, "}");
    // Strip simple parameter/variable primitive/named type annotations in function signatures.
    code = code.replace(/(\(\s*|\,\s*)(\w+)\??\s*:\s*(?:string|number|boolean|any|unknown|void|never|[A-Z]\w*(?:<[^,)]*>)?)(?:\[\])?(?=\s*[,)=])/g, "$1$2");
    // Strip `as SomeType` assertions in simple expressions.
    code = code.replace(/\s+as\s+(?:const|[A-Z]\w*(?:<[^>]+>)?(?:\[\])?)/g, "");
    // Rewrite `export default` and named `export`.
    let defaultExpr = null;
    code = code.replace(/\bexport\s+default\s+function\s+(\w+)/g, (_, name) => { defaultExpr = name; return `function ${name}`; });
    code = code.replace(/\bexport\s+default\s+function\s*\(/g, () => { defaultExpr = "__DomovoiDefault"; return "function __DomovoiDefault("; });
    code = code.replace(/\bexport\s+default\s+class\s+(\w+)/g, (_, name) => { defaultExpr = name; return `class ${name}`; });
    code = code.replace(/\bexport\s+default\s+/g, () => { defaultExpr = "__DomovoiDefault"; return "const __DomovoiDefault = "; });
    code = code.replace(/\bexport\s+(?=(?:const|let|var|function|class)\b)/g, "");
    return { code: preamble.join("\n") + "\n" + code, defaultExpr };
  }

  function transpileJSX(input) {
    let i = 0;
    const len = input.length;

    function isIdentStart(ch) { return /[A-Za-z_$]/.test(ch); }
    function isIdentPart(ch) { return /[A-Za-z0-9_$.-]/.test(ch); }

    function skipString(quote) {
      let out = quote; i++;
      while (i < len) {
        const ch = input[i++];
        out += ch;
        if (ch === "\\" && i < len) { out += input[i++]; continue; }
        if (ch === quote) break;
      }
      return out;
    }

    function skipTemplate() {
      let out = "`"; i++;
      while (i < len) {
        const ch = input[i++];
        out += ch;
        if (ch === "\\" && i < len) { out += input[i++]; continue; }
        if (ch === "$" && input[i] === "{") {
          out += "{"; i++;
          out += parseCode("}");
          if (input[i] === "}") { out += "}"; i++; }
          continue;
        }
        if (ch === "`") break;
      }
      return out;
    }

    function canStartJSX(prevToken) {
      if (!prevToken) return true;
      return ["=", "(", ",", ":", "?", "return", "=>", "{", "[", "&&", "||", "!", ";", "\n"].includes(prevToken);
    }

    function lookaheadJSXTag() {
      let j = i + 1;
      if (input[j] === ">") return true; // Fragment <>
      if (!isIdentStart(input[j] || "")) return false;
      while (j < len && isIdentPart(input[j])) j++;
      while (j < len && /\s/.test(input[j])) j++;
      const ch = input[j];
      return ch === ">" || ch === "/" || isIdentStart(ch || "") || ch === "{";
    }

    function parseJSX() {
      i++; // consume '<'
      let tag = "";
      let isFragment = false;
      if (input[i] === ">") {
        isFragment = true;
        i++;
      } else {
        while (i < len && isIdentPart(input[i])) tag += input[i++];
      }

      const propParts = [];
      let selfClosing = false;
      if (!isFragment) {
        while (i < len) {
          while (i < len && /\s/.test(input[i])) i++;
          if (input[i] === "/" && input[i + 1] === ">") { selfClosing = true; i += 2; break; }
          if (input[i] === ">") { i++; break; }
          if (input[i] === "{" && input.slice(i + 1, i + 4) === "...") {
            i += 4;
            const spread = parseCode("}");
            if (input[i] === "}") i++;
            propParts.push("...(" + spread + ")");
            continue;
          }
          let attr = "";
          while (i < len && /[A-Za-z0-9_:-]/.test(input[i])) attr += input[i++];
          if (!attr) { i++; continue; }
          while (i < len && /\s/.test(input[i])) i++;
          let val = "true";
          if (input[i] === "=") {
            i++;
            while (i < len && /\s/.test(input[i])) i++;
            if (input[i] === '"' || input[i] === "'") {
              val = skipString(input[i]);
            } else if (input[i] === "{") {
              i++;
              val = "(" + parseCode("}") + ")";
              if (input[i] === "}") i++;
            }
          }
          propParts.push(JSON.stringify(attr) + ":" + val);
        }
      }

      const children = [];
      if (!selfClosing) {
        let textBuf = "";
        const flushText = () => {
          if (!textBuf) return;
          const cleaned = textBuf.replace(/\n\s*/g, " ");
          if (cleaned.trim().length > 0) children.push(JSON.stringify(cleaned));
          textBuf = "";
        };
        while (i < len) {
          if (input[i] === "<" && input[i + 1] === "/") {
            flushText();
            i += 2;
            while (i < len && input[i] !== ">") i++;
            if (input[i] === ">") i++;
            break;
          }
          if (input[i] === "<" && lookaheadJSXTag()) {
            flushText();
            children.push(parseJSX());
            continue;
          }
          if (input[i] === "{") {
            flushText();
            i++;
            const expr = parseCode("}").trim();
            if (input[i] === "}") i++;
            if (expr && !/^\/\*[\s\S]*\*\/$/.test(expr)) children.push("(" + expr + ")");
            continue;
          }
          textBuf += input[i++];
        }
      }

      const tagExpr = isFragment
        ? "React.Fragment"
        : (/^[A-Z]/.test(tag) || tag.includes(".")) ? tag : JSON.stringify(tag);
      const propsExpr = propParts.length ? "{" + propParts.join(",") + "}" : "null";
      return "React.createElement(" + tagExpr + "," + propsExpr + (children.length ? "," + children.join(",") : "") + ")";
    }

    function parseCode(stopChar) {
      let out = "";
      let braceDepth = 0;
      let prevToken = "";
      while (i < len) {
        const ch = input[i];
        if (stopChar && ch === stopChar && braceDepth === 0) break;
        if (ch === "'" || ch === '"') { out += skipString(ch); prevToken = "lit"; continue; }
        if (ch === "`") { out += skipTemplate(); prevToken = "lit"; continue; }
        if (ch === "/" && input[i + 1] === "/") {
          while (i < len && input[i] !== "\n") out += input[i++];
          continue;
        }
        if (ch === "/" && input[i + 1] === "*") {
          out += "/*"; i += 2;
          while (i < len && !(input[i] === "*" && input[i + 1] === "/")) out += input[i++];
          if (i < len) { out += "*/"; i += 2; }
          continue;
        }
        if (ch === "<" && canStartJSX(prevToken) && lookaheadJSXTag()) {
          out += parseJSX();
          prevToken = "jsx";
          continue;
        }
        if (ch === "{") { braceDepth++; out += ch; i++; prevToken = "{"; continue; }
        if (ch === "}") { braceDepth--; out += ch; i++; prevToken = "}"; continue; }
        if (isIdentStart(ch)) {
          let word = "";
          while (i < len && /[A-Za-z0-9_$]/.test(input[i])) word += input[i++];
          out += word;
          prevToken = word;
          continue;
        }
        if (ch === "=" && input[i + 1] === ">") { out += "=>"; i += 2; prevToken = "=>"; continue; }
        if ((ch === "&" && input[i + 1] === "&") || (ch === "|" && input[i + 1] === "|")) {
          out += ch + ch; i += 2; prevToken = ch + ch; continue;
        }
        if (/\s/.test(ch)) {
          if (ch === "\n" && prevToken === "return") prevToken = "\n";
          out += ch; i++; continue;
        }
        out += ch; i++;
        prevToken = ch;
      }
      return out;
    }

    return parseCode(null);
  }

  function mountArtifact() {
    const srcEl = document.getElementById("domovoi-jsx-source");
    const rootEl = document.getElementById("root");
    if (!srcEl || !rootEl) return;
    const raw = srcEl.textContent || "";
    try {
      const { code, defaultExpr } = stripTypesAndImports(raw);
      const compiled = transpileJSX(code);
      const returnStmt = defaultExpr
        ? `\nreturn typeof ${defaultExpr} !== "undefined" ? ${defaultExpr} : null;`
        : `\nreturn typeof App !== "undefined" ? App : (typeof Component !== "undefined" ? Component : null);`;
      const fn = new Function("React", "__lucide", compiled + returnStmt);
      const Comp = fn(React, lucideProxy);
      if (!Comp) {
        throw new Error("No default export or App component found.");
      }
      const root = createRoot(rootEl);
      root.render(typeof Comp === "function" ? createElement(Comp, null) : Comp);
    } catch (err) {
      const msg = err && err.message ? err.message : String(err);
      reportError(msg, 0);
      rootEl.innerHTML = "";
      const box = document.createElement("div");
      box.className = "domovoi-runtime-error";
      box.textContent = "Preview error: " + msg;
      rootEl.appendChild(box);
    }
  }

  window.React = React;
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", mountArtifact);
  else mountArtifact();
})();

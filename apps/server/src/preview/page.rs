//! What runs in a server tab's page, and how agents name its elements
//! (t3code `ServerBrowserPage.ts`, which uses Playwright locators and its
//! aria snapshot).
//!
//! A snapshot lists the page's visible interactive elements, each with a CSS
//! path and a ref (`e1`, `e2`, ...); `locator: "aria-ref=e3"` finds the
//! element again until the next snapshot, navigation or control handoff.
//! Locators also take `css=`, `text=` (substring, or exact when quoted),
//! `role=<role>[name="..."]` and plain CSS.
//!
//! Limits: no shadow roots or frames; the accessibility tree is a flat list
//! of roles and names, not Playwright's aria snapshot; accessible names are
//! approximated (label, aria-label, text, placeholder, title, alt).

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "page scripts take and return JSON")]

use serde_json::{Value, json};

/// How agents pick an element.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Css(String),
    Text { text: String, exact: bool },
    Role { role: String, name: Option<String> },
}

/// Why a locator names nothing it could find.
#[derive(Debug, PartialEq)]
pub enum LocatorError {
    /// `aria-ref=` of an earlier snapshot.
    StaleRef,
    Invalid,
}

impl Target {
    /// The JSON the page scripts read.
    pub fn spec(&self) -> Value {
        match self {
            Target::Css(css) => json!({ "kind": "css", "value": css }),
            Target::Text { text, exact } => json!({ "kind": "text", "value": text, "exact": exact }),
            Target::Role { role, name } => json!({ "kind": "role", "value": role, "name": name }),
        }
    }
}

fn unquote(s: &str) -> (String, bool) {
    let t = s.trim();
    for q in ['"', '\''] {
        if t.len() >= 2 && t.starts_with(q) && t.ends_with(q) {
            return (t[1..t.len() - 1].to_owned(), true);
        }
    }
    (t.to_owned(), false)
}

/// Reads a `locator` (`refs`: the CSS paths of the latest snapshot, by ref number).
pub fn parse_locator(locator: &str, refs: &[String]) -> Result<Target, LocatorError> {
    let l = locator.trim();
    if let Some(r) = l.strip_prefix("aria-ref=") {
        let n: usize = r.trim().strip_prefix('e').and_then(|n| n.parse().ok()).ok_or(LocatorError::StaleRef)?;
        return refs.get(n.wrapping_sub(1)).map(|css| Target::Css(css.clone())).ok_or(LocatorError::StaleRef);
    }
    if let Some(css) = l.strip_prefix("css=") {
        return Ok(Target::Css(css.trim().to_owned()));
    }
    if let Some(text) = l.strip_prefix("text=") {
        let (text, exact) = unquote(text);
        return if text.is_empty() { Err(LocatorError::Invalid) } else { Ok(Target::Text { text, exact }) };
    }
    if let Some(rest) = l.strip_prefix("role=") {
        let (role, name) = match rest.split_once('[') {
            Some((role, attrs)) => {
                let attrs = attrs.strip_suffix(']').ok_or(LocatorError::Invalid)?;
                let value = attrs.trim().strip_prefix("name=").ok_or(LocatorError::Invalid)?;
                (role.trim(), Some(unquote(value).0))
            }
            None => (rest.trim(), None),
        };
        if role.is_empty() || !role.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(LocatorError::Invalid);
        }
        return Ok(Target::Role { role: role.to_ascii_lowercase(), name });
    }
    if l.is_empty() { Err(LocatorError::Invalid) } else { Ok(Target::Css(l.to_owned())) }
}

/// Helpers every element script starts with: `find(spec)` (an element, null,
/// or throws `invalid` for a selector the page cannot parse) and `role` /
/// `nameOf` (approximate accessible role and name).
const HELPERS: &str = r#"
const roleOf = (el) => {
  const explicit = el.getAttribute("role");
  if (explicit) return explicit.split(/\s+/)[0];
  const tag = el.localName;
  const type = (el.getAttribute("type") || "text").toLowerCase();
  if (tag === "a") return el.hasAttribute("href") ? "link" : null;
  if (tag === "button" || tag === "summary") return "button";
  if (tag === "select") return el.multiple ? "listbox" : "combobox";
  if (tag === "textarea") return "textbox";
  if (tag === "img") return "img";
  if (/^h[1-6]$/.test(tag)) return "heading";
  if (tag === "input") {
    if (["button", "submit", "reset", "image"].includes(type)) return "button";
    if (type === "checkbox") return "checkbox";
    if (type === "radio") return "radio";
    if (type === "range") return "slider";
    if (type === "number") return "spinbutton";
    if (type === "search") return "searchbox";
    return "textbox";
  }
  if (el.isContentEditable) return "textbox";
  return null;
};
const clean = (s) => (s || "").replace(/\s+/g, " ").trim();
const nameOf = (el) => {
  const label = el.getAttribute("aria-label");
  if (label) return clean(label);
  const by = el.getAttribute("aria-labelledby");
  if (by) {
    const text = by.split(/\s+/).map((id) => document.getElementById(id)?.innerText || "").join(" ");
    if (clean(text)) return clean(text);
  }
  if (el.labels && el.labels.length) return clean([...el.labels].map((l) => l.innerText).join(" "));
  const tag = el.localName;
  if (tag === "input" && ["button", "submit", "reset"].includes((el.type || "").toLowerCase())) return clean(el.value);
  if (tag === "img") return clean(el.alt);
  if (tag !== "input" && tag !== "textarea" && tag !== "select") {
    const text = clean(el.innerText || el.textContent);
    if (text) return text.slice(0, 200);
  }
  return clean(el.getAttribute("placeholder") || el.getAttribute("title") || el.getAttribute("alt") || "");
};
const visible = (el) => {
  const r = el.getBoundingClientRect();
  if (r.width <= 0 || r.height <= 0) return false;
  const s = getComputedStyle(el);
  return s.visibility !== "hidden" && s.display !== "none";
};
const find = (spec) => {
  if (spec.kind === "css") {
    try { return document.querySelector(spec.value); } catch (e) { throw "invalid"; }
  }
  const all = [...document.querySelectorAll("body *")];
  if (spec.kind === "text") {
    const matches = all.filter((el) => {
      const text = clean(el.innerText);
      return spec.exact ? text === spec.value : text.includes(spec.value);
    });
    // The innermost element that holds the text.
    return matches.find((el) => !matches.some((other) => other !== el && el.contains(other))) || null;
  }
  return all.find((el) => roleOf(el) === spec.value && visible(el) && (spec.name == null || nameOf(el) === spec.name || nameOf(el).includes(spec.name))) || null;
};
"#;

/// `body` run with the helpers in scope, `spec` (a target, or null) and `arg`.
/// A selector the page cannot parse answers `{ invalid: true }`.
pub fn element_script(body: &str, spec: &Value, arg: &Value) -> String {
    format!("(() => {{ {HELPERS}\n const run = (spec, arg) => {{ {body} }};\n try {{ return run({spec}, {arg}); }} catch (e) {{ if (e === \"invalid\") return {{ invalid: true }}; throw e; }} }})()")
}

/// Scrolls the element into view and answers its centre, in CSS pixels.
pub const POINT: &str = r#"
const el = find(spec);
if (!el) return { found: false };
el.scrollIntoView({ block: "center", inline: "center", behavior: "instant" });
const r = el.getBoundingClientRect();
return { found: true, x: r.x + r.width / 2, y: r.y + r.height / 2 };
"#;

/// Focuses an enabled text field (the element, or the focused one) and
/// selects its text when `arg.clear`; `false` when it is not one.
pub const FOCUS_TEXT: &str = r#"
let el = spec ? find(spec) : document.activeElement;
if (spec && !el) return { found: false };
while (!spec && el && el.shadowRoot && el.shadowRoot.activeElement) el = el.shadowRoot.activeElement;
if (!el) return { found: true, editable: false };
const nonText = ["button", "checkbox", "color", "file", "hidden", "image", "radio", "range", "reset", "submit"];
const textControl = el.localName === "textarea" || (el.localName === "input" && !nonText.includes((el.type || "text").toLowerCase()));
if (!(textControl || el.isContentEditable) || el.disabled || el.readOnly) return { found: true, editable: false };
el.focus();
if (arg && arg.clear) {
  if (textControl) el.select();
  else { const range = document.createRange(); range.selectNodeContents(el); const sel = getSelection(); sel.removeAllRanges(); sel.addRange(range); }
}
return { found: true, editable: true };
"#;

/// Selects `arg.values` (by value or label) in a native `<select>`.
pub const SELECT: &str = r#"
const el = find(spec);
if (!el) return { found: false };
if (el.localName !== "select") return { found: true, notSelect: true };
const wanted = arg.values;
let picked = false;
for (const option of el.options) {
  const on = wanted.includes(option.value) || wanted.includes(clean(option.label)) || wanted.includes(clean(option.text));
  if (on && !el.multiple && picked) { option.selected = false; continue; }
  option.selected = on;
  picked = picked || on;
}
el.dispatchEvent(new Event("input", { bubbles: true }));
el.dispatchEvent(new Event("change", { bubbles: true }));
return { found: true, selected: [...el.selectedOptions].map((o) => o.value) };
"#;

/// Scrolls the element (or the page) by `arg.dx`, `arg.dy`.
pub const SCROLL: &str = r#"
const el = spec ? find(spec) : null;
if (spec && !el) return { found: false };
(el || document.scrollingElement || document.documentElement).scrollBy({ left: arg.dx, top: arg.dy, behavior: "instant" });
return { found: true };
"#;

/// Whether the element exists.
pub const EXISTS: &str = "return { found: !!find(spec) };";

/// The element itself, for `DOM.setFileInputFiles` (evaluated by reference).
pub const ELEMENT: &str = "const el = find(spec); if (!el) return null; if (el.localName !== 'input' || el.type !== 'file') return 'not-file'; return el;";

/// What a snapshot reads: the page's text, title, readiness and its visible
/// interactive elements with CSS paths.
pub fn snapshot_script(max_text: usize, max_elements: usize) -> String {
    format!(
        r##"(() => {{ {HELPERS}
const path = (el) => {{
  const parts = [];
  for (let e = el; e && e.nodeType === 1; e = e.parentElement) {{
    if (e.id && document.querySelectorAll("#" + CSS.escape(e.id)).length === 1) {{ parts.unshift("#" + CSS.escape(e.id)); break; }}
    let part = e.localName;
    const parent = e.parentElement;
    if (parent) {{
      const same = [...parent.children].filter((c) => c.localName === e.localName);
      if (same.length > 1) part += ":nth-of-type(" + (same.indexOf(e) + 1) + ")";
    }}
    parts.unshift(part);
    if (e.localName === "html") break;
  }}
  return parts.join(" > ");
}};
const query = 'a[href], button, input:not([type="hidden"]), select, textarea, summary, [role="button"], [role="link"], [role="checkbox"], [role="radio"], [role="tab"], [role="menuitem"], [role="option"], [role="switch"], [role="textbox"], [role="combobox"], [contenteditable=""], [contenteditable="true"], h1, h2, h3, [tabindex]:not([tabindex="-1"])';
const elements = [];
for (const el of document.querySelectorAll(query)) {{
  if (elements.length >= {max_elements}) break;
  if (!visible(el)) continue;
  const r = el.getBoundingClientRect();
  elements.push({{ tag: el.localName, role: roleOf(el), name: nameOf(el), selector: path(el), x: Math.round(r.x), y: Math.round(r.y), width: Math.round(r.width), height: Math.round(r.height) }});
}}
return {{ url: location.href, title: document.title, loading: document.readyState !== "complete", visibleText: ((document.body && document.body.innerText) || "").slice(0, {max_text}), interactiveElements: elements }};
}})()"##
    )
}

/// The flat accessibility list a snapshot carries: one line per element, with its ref.
pub fn tree(elements: &[Value]) -> String {
    elements
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let role = e["role"].as_str().unwrap_or_else(|| e["tag"].as_str().unwrap_or("generic"));
            let name = e["name"].as_str().unwrap_or_default();
            if name.is_empty() { format!("- {role} [ref=e{}]", i + 1) } else { format!("- {role} {} [ref=e{}]", json!(name), i + 1) }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `Input.dispatchKeyEvent` fields for a key Playwright names (`Enter`, `a`, `ArrowDown`).
pub fn key_event(key: &str) -> (String, String, i64, Option<String>) {
    let named = |code: &str, vk: i64, text: Option<&str>| (key.to_owned(), code.to_owned(), vk, text.map(str::to_owned));
    match key {
        "Enter" => named("Enter", 13, Some("\r")),
        "Tab" => named("Tab", 9, None),
        "Escape" => named("Escape", 27, None),
        "Backspace" => named("Backspace", 8, None),
        "Delete" => named("Delete", 46, None),
        "ArrowUp" => named("ArrowUp", 38, None),
        "ArrowDown" => named("ArrowDown", 40, None),
        "ArrowLeft" => named("ArrowLeft", 37, None),
        "ArrowRight" => named("ArrowRight", 39, None),
        "Home" => named("Home", 36, None),
        "End" => named("End", 35, None),
        "PageUp" => named("PageUp", 33, None),
        "PageDown" => named("PageDown", 34, None),
        " " | "Space" => (" ".into(), "Space".into(), 32, Some(" ".into())),
        _ => {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => {
                    let upper = c.to_ascii_uppercase();
                    let code = if c.is_ascii_alphabetic() { format!("Key{upper}") } else if c.is_ascii_digit() { format!("Digit{c}") } else { String::new() };
                    (key.to_owned(), code, upper as i64, Some(key.to_owned()))
                }
                _ => (key.to_owned(), key.to_owned(), 0, None),
            }
        }
    }
}

/// CDP modifier bits for Playwright's names.
pub fn modifier_bits(names: &[String]) -> i64 {
    names.iter().map(|m| match m.as_str() {
        "Alt" => 1,
        "Control" => 2,
        "Meta" => 4,
        "Shift" => 8,
        _ => 0,
    }).sum()
}

/// `withoutDataPayload`: a `data:` URL keeps only its media type.
pub fn without_data_payload(url: &str) -> String {
    let lower = url.trim_start().to_ascii_lowercase();
    if !lower.starts_with("data:") {
        return url.to_owned();
    }
    let rest = &url.trim_start()[5..];
    let kind = rest.split([',', ';']).next().unwrap_or("").trim();
    let ok = kind.split_once('/').is_some_and(|(a, b)| {
        let part = |s: &str| !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c));
        part(a) && part(b)
    });
    format!("data:{},…", if ok { kind } else { "" })
}

/// `parseNetError`: a Chromium net error's name and number.
pub fn net_error(error_text: &str) -> (String, i64) {
    let name = error_text.find("ERR_").map(|i| error_text[i..].chars().take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_').collect::<String>()).unwrap_or_else(|| error_text.to_owned());
    let code = match name.as_str() {
        "ERR_FAILED" => -2,
        "ERR_ABORTED" => -3,
        "ERR_TIMED_OUT" => -7,
        "ERR_CONNECTION_CLOSED" => -100,
        "ERR_CONNECTION_RESET" => -101,
        "ERR_CONNECTION_REFUSED" => -102,
        "ERR_NAME_NOT_RESOLVED" => -105,
        "ERR_INTERNET_DISCONNECTED" => -106,
        "ERR_ADDRESS_UNREACHABLE" => -109,
        "ERR_CERT_AUTHORITY_INVALID" => -202,
        "ERR_EMPTY_RESPONSE" => -324,
        _ => -2,
    };
    (name, code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locators_read_like_playwright_ones() {
        let refs = vec!["#go".to_owned(), "main > a:nth-of-type(2)".to_owned()];
        assert_eq!(parse_locator("aria-ref=e2", &refs), Ok(Target::Css("main > a:nth-of-type(2)".into())));
        assert_eq!(parse_locator("aria-ref=e3", &refs), Err(LocatorError::StaleRef));
        assert_eq!(parse_locator("aria-ref=x", &refs), Err(LocatorError::StaleRef));
        assert_eq!(parse_locator("text=\"Sign in\"", &refs), Ok(Target::Text { text: "Sign in".into(), exact: true }));
        assert_eq!(parse_locator("text=Sign", &refs), Ok(Target::Text { text: "Sign".into(), exact: false }));
        assert_eq!(parse_locator("role=button[name=\"Save\"]", &refs), Ok(Target::Role { role: "button".into(), name: Some("Save".into()) }));
        assert_eq!(parse_locator("role=link", &refs), Ok(Target::Role { role: "link".into(), name: None }));
        assert_eq!(parse_locator("role=but ton", &refs), Err(LocatorError::Invalid));
        assert_eq!(parse_locator("css=#a", &refs), Ok(Target::Css("#a".into())));
        assert_eq!(parse_locator("form input", &refs), Ok(Target::Css("form input".into())));
    }

    #[test]
    fn keys_urls_and_net_errors() {
        assert_eq!(key_event("Enter"), ("Enter".into(), "Enter".into(), 13, Some("\r".into())));
        assert_eq!(key_event("a"), ("a".into(), "KeyA".into(), 65, Some("a".into())));
        assert_eq!(modifier_bits(&["Control".into(), "Shift".into()]), 10);
        assert_eq!(without_data_payload("data:text/html;base64,PGh0bWw+"), "data:text/html,…");
        assert_eq!(without_data_payload("data:<x>,y"), "data:,…");
        assert_eq!(without_data_payload("http://a/"), "http://a/");
        assert_eq!(net_error("net::ERR_CONNECTION_REFUSED"), ("ERR_CONNECTION_REFUSED".into(), -102));
        assert_eq!(net_error("net::ERR_HTTP2_PROTOCOL_ERROR").1, -2);
        let elements = vec![json!({ "tag": "button", "role": "button", "name": "Go" }), json!({ "tag": "div", "role": null, "name": "" })];
        assert_eq!(tree(&elements), "- button \"Go\" [ref=e1]\n- div [ref=e2]");
    }
}

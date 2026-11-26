//! Lightweight top-level symbol extraction (brace-depth and indentation heuristics).

use super::IndexedSymbol;

struct ParsedSymbol {
    name: String,
    kind: &'static str,
    start_line: i64,
    end_line: i64,
    signature: Option<String>,
    is_exported: bool,
}

pub fn extract_symbols(path: &str, language: &str, content: &str) -> Vec<IndexedSymbol> {
    let lines: Vec<&str> = content.lines().collect();
    let parsed = match language {
        "rust" => extract_brace(&lines, parse_rust, "//"),
        "typescript" | "typescriptreact" | "javascript" | "javascriptreact" => extract_brace(&lines, parse_ts_js, "//"),
        "go" => extract_brace(&lines, parse_go, "//"),
        "swift" => extract_brace(&lines, parse_swift, "//"),
        "kotlin" => extract_brace(&lines, parse_kotlin, "//"),
        "python" => extract_python(&lines),
        "ruby" => extract_ruby(&lines),
        _ => Vec::new(),
    };
    parsed
        .into_iter()
        .map(|s| IndexedSymbol {
            path: path.to_owned(),
            name: s.name,
            kind: s.kind.to_owned(),
            start_line: s.start_line,
            end_line: s.end_line,
            signature: s.signature,
            is_exported: s.is_exported,
        })
        .collect()
}

fn extract_brace(lines: &[&str], parse: fn(&str) -> Option<(String, &'static str, bool)>, comment_prefix: &str) -> Vec<ParsedSymbol> {
    let mut out = Vec::new();
    let mut depth: i64 = 0;
    for (idx, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        if depth == 0 && !line.is_empty() && !line.starts_with(comment_prefix)
            && let Some((name, kind, is_exported)) = parse(line)
        {
            let start_line = idx as i64 + 1;
            let end_line = find_brace_end(lines, idx, start_line);
            out.push(ParsedSymbol {
                name,
                kind,
                start_line,
                end_line,
                signature: Some(line.chars().take(240).collect()),
                is_exported,
            });
        }
        let code = line.split(comment_prefix).next().unwrap_or(line);
        depth += code.bytes().filter(|&b| b == b'{').count() as i64;
        depth -= code.bytes().filter(|&b| b == b'}').count() as i64;
        if depth < 0 {
            depth = 0;
        }
    }
    out
}

fn find_brace_end(lines: &[&str], start_idx: usize, start_line: i64) -> i64 {
    let mut depth: i64 = 0;
    let mut seen_open = false;
    for (offset, raw) in lines[start_idx..].iter().take(400).enumerate() {
        for b in raw.bytes() {
            if b == b'{' {
                depth += 1;
                seen_open = true;
            } else if b == b'}' {
                depth -= 1;
                if seen_open && depth <= 0 {
                    return start_line + offset as i64;
                }
            }
        }
        if !seen_open && raw.trim_end().ends_with(';') {
            return start_line + offset as i64;
        }
    }
    start_line
}

fn ident_head(rest: &str) -> Option<String> {
    let name: String = rest.trim_start().chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$').collect();
    (!name.is_empty()).then_some(name)
}

fn parse_rust(line: &str) -> Option<(String, &'static str, bool)> {
    let (exported, rest) = if let Some(r) = line.strip_prefix("pub(crate) ") {
        (false, r)
    } else if let Some(r) = line.strip_prefix("pub(super) ") {
        (false, r)
    } else if let Some(r) = line.strip_prefix("pub ") {
        (true, r)
    } else {
        (false, line)
    };
    let rest = rest.strip_prefix("async ").unwrap_or(rest);
    let rest = rest.strip_prefix("unsafe ").unwrap_or(rest);
    let rest = rest.strip_prefix("const ").unwrap_or(rest);
    for (kw, kind) in [("fn ", "function"), ("struct ", "struct"), ("enum ", "enum"), ("trait ", "trait"), ("type ", "type"), ("mod ", "module")] {
        if let Some(tail) = rest.strip_prefix(kw) {
            return Some((ident_head(tail)?, kind, exported));
        }
    }
    None
}

fn parse_ts_js(line: &str) -> Option<(String, &'static str, bool)> {
    let (exported, rest) = if let Some(r) = line.strip_prefix("export default ") {
        (true, r)
    } else if let Some(r) = line.strip_prefix("export ") {
        (true, r)
    } else {
        (false, line)
    };
    let rest = rest.strip_prefix("async ").unwrap_or(rest);
    let rest = rest.strip_prefix("abstract ").unwrap_or(rest);
    for (kw, kind) in [
        ("function ", "function"),
        ("class ", "class"),
        ("interface ", "interface"),
        ("type ", "type"),
        ("enum ", "enum"),
        ("const ", "const"),
    ] {
        if let Some(tail) = rest.strip_prefix(kw) {
            return Some((ident_head(tail)?, kind, exported));
        }
    }
    None
}

fn parse_go(line: &str) -> Option<(String, &'static str, bool)> {
    if let Some(rest) = line.strip_prefix("func ") {
        let rest = if rest.starts_with('(') { rest.split_once(')')?.1.trim_start() } else { rest };
        let name = ident_head(rest)?;
        let exported = name.chars().next().is_some_and(char::is_uppercase);
        return Some((name, "function", exported));
    }
    if let Some(rest) = line.strip_prefix("type ") {
        let name = ident_head(rest)?;
        let exported = name.chars().next().is_some_and(char::is_uppercase);
        return Some((name, "type", exported));
    }
    None
}

fn parse_swift(line: &str) -> Option<(String, &'static str, bool)> {
    let mut rest = line;
    let mut exported = false;
    loop {
        if let Some(r) = rest.strip_prefix("public ").or_else(|| rest.strip_prefix("open ")) {
            exported = true;
            rest = r;
        } else if let Some(r) = rest
            .strip_prefix("private ")
            .or_else(|| rest.strip_prefix("fileprivate "))
            .or_else(|| rest.strip_prefix("internal "))
            .or_else(|| rest.strip_prefix("final "))
            .or_else(|| rest.strip_prefix("static "))
            .or_else(|| rest.strip_prefix("override "))
        {
            rest = r;
        } else {
            break;
        }
    }
    for (kw, kind) in [("func ", "function"), ("struct ", "struct"), ("class ", "class"), ("enum ", "enum"), ("protocol ", "protocol"), ("actor ", "actor")] {
        if let Some(tail) = rest.strip_prefix(kw) {
            return Some((ident_head(tail)?, kind, exported));
        }
    }
    None
}

fn parse_kotlin(line: &str) -> Option<(String, &'static str, bool)> {
    let mut rest = line;
    let mut exported = true;
    loop {
        if let Some(r) = rest.strip_prefix("private ").or_else(|| rest.strip_prefix("internal ")).or_else(|| rest.strip_prefix("protected ")) {
            exported = false;
            rest = r;
        } else if let Some(r) = rest
            .strip_prefix("public ")
            .or_else(|| rest.strip_prefix("suspend ") )
            .or_else(|| rest.strip_prefix("inline "))
            .or_else(|| rest.strip_prefix("data "))
            .or_else(|| rest.strip_prefix("sealed "))
            .or_else(|| rest.strip_prefix("abstract "))
            .or_else(|| rest.strip_prefix("open "))
            .or_else(|| rest.strip_prefix("override "))
        {
            rest = r;
        } else {
            break;
        }
    }
    for (kw, kind) in [("fun ", "function"), ("class ", "class"), ("interface ", "interface"), ("object ", "object"), ("enum class ", "enum")] {
        if let Some(tail) = rest.strip_prefix(kw) {
            return Some((ident_head(tail)?, kind, exported));
        }
    }
    None
}

fn extract_python(lines: &[&str]) -> Vec<ParsedSymbol> {
    let mut out = Vec::new();
    for (idx, raw) in lines.iter().enumerate() {
        if raw.starts_with(' ') || raw.starts_with('\t') {
            continue;
        }
        let line = raw.trim();
        let rest = line.strip_prefix("async ").unwrap_or(line);
        for (kw, kind) in [("def ", "function"), ("class ", "class")] {
            if let Some(tail) = rest.strip_prefix(kw)
                && let Some(name) = ident_head(tail)
            {
                let start_line = idx as i64 + 1;
                let mut end_line = start_line;
                for (off, next) in lines[idx + 1..].iter().enumerate() {
                    if !next.trim().is_empty() && !next.starts_with(' ') && !next.starts_with('\t') {
                        break;
                    }
                    if !next.trim().is_empty() {
                        end_line = start_line + off as i64 + 1;
                    }
                }
                let is_exported = !name.starts_with('_');
                out.push(ParsedSymbol { name, kind, start_line, end_line, signature: Some(line.chars().take(240).collect()), is_exported });
            }
        }
    }
    out
}

fn extract_ruby(lines: &[&str]) -> Vec<ParsedSymbol> {
    let mut out = Vec::new();
    for (idx, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        for (kw, kind) in [("def ", "function"), ("class ", "class"), ("module ", "module")] {
            if let Some(tail) = line.strip_prefix(kw)
                && let Some(name) = ident_head(tail)
            {
                let start_line = idx as i64 + 1;
                out.push(ParsedSymbol { name, kind, start_line, end_line: start_line, signature: Some(line.chars().take(240).collect()), is_exported: true });
            }
        }
    }
    out
}

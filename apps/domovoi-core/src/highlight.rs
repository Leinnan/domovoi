//! Syntax highlighting for code blocks and file previews: a small tokenizer
//! (comments, strings, numbers, keywords, types, calls) per language family,
//! not a grammar. It is meant to make code easier to read, not to be exact,
//! and it stays small enough for the phone.

use crate::view::CodeSpan;

/// Code longer than this shows plain (highlighting it costs more than it helps).
const MAX_BYTES: usize = 48 * 1024;

struct Lang {
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    /// Quote characters; a string ends at the same unescaped quote.
    quotes: &'static [char],
    keywords: &'static [&'static str],
    /// Capitalized words are types (Rust, Swift, Go, Java…).
    capitalized_types: bool,
}

const C_LIKE_COMMENTS: &[&str] = &["//"];
const RUST: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let",
    "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe",
    "use", "where", "while", "Some", "None", "Ok", "Err",
];
const SWIFT: &[&str] = &[
    "actor", "any", "as", "async", "await", "break", "case", "catch", "class", "continue", "default", "defer", "do", "else", "enum", "extension",
    "fallthrough", "false", "fileprivate", "for", "func", "guard", "if", "import", "in", "init", "inout", "internal", "is", "let", "nil", "open",
    "private", "protocol", "public", "repeat", "return", "self", "Self", "some", "static", "struct", "super", "switch", "throw", "throws", "true",
    "try", "var", "where", "while", "@MainActor", "@State", "@Published", "@ObservedObject",
];
const JS: &[&str] = &[
    "as", "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete", "do", "else", "enum",
    "export", "extends", "false", "finally", "for", "from", "function", "if", "implements", "import", "in", "instanceof", "interface", "let",
    "new", "null", "of", "private", "protected", "public", "readonly", "return", "static", "super", "switch", "this", "throw", "true", "try",
    "type", "typeof", "undefined", "var", "void", "while", "yield",
];
const PYTHON: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "False", "finally", "for",
    "from", "global", "if", "import", "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "self", "True", "try",
    "while", "with", "yield",
];
const GO: &[&str] = &[
    "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "false", "for", "func", "go", "goto", "if",
    "import", "interface", "map", "nil", "package", "range", "return", "select", "struct", "switch", "true", "type", "var",
];
const C_FAMILY: &[&str] = &[
    "abstract", "auto", "bool", "break", "case", "catch", "char", "class", "const", "continue", "default", "delete", "do", "double", "else",
    "enum", "extends", "extern", "false", "final", "float", "for", "fun", "if", "implements", "import", "include", "int", "interface", "long",
    "namespace", "new", "null", "nullptr", "override", "package", "private", "protected", "public", "return", "short", "signed", "sizeof",
    "static", "struct", "super", "switch", "template", "this", "throw", "throws", "true", "try", "typedef", "union", "unsigned", "using", "val",
    "var", "virtual", "void", "volatile", "when", "while",
];
const RUBY: &[&str] = &[
    "begin", "break", "case", "class", "def", "do", "else", "elsif", "end", "ensure", "false", "for", "if", "in", "module", "next", "nil",
    "not", "or", "and", "redo", "rescue", "retry", "return", "self", "super", "then", "true", "unless", "until", "when", "while", "yield",
];
const SHELL: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "in", "do", "done", "while", "until", "case", "esac", "function", "return", "export", "local",
    "set", "unset", "echo", "cd", "exit", "source", "sudo",
];
const SQL: &[&str] = &[
    "select", "from", "where", "insert", "into", "values", "update", "set", "delete", "create", "table", "drop", "alter", "index", "join",
    "left", "right", "inner", "outer", "on", "and", "or", "not", "null", "as", "group", "by", "order", "having", "limit", "primary", "key",
    "default", "distinct", "union", "case", "when", "then", "else", "end", "SELECT", "FROM", "WHERE", "INSERT", "INTO", "VALUES", "UPDATE",
    "SET", "DELETE", "CREATE", "TABLE", "JOIN", "ON", "AND", "OR", "NOT", "NULL", "AS", "GROUP", "BY", "ORDER", "LIMIT", "PRIMARY", "KEY",
];
const DATA: &[&str] = &["true", "false", "null", "yes", "no", "on", "off"];

fn lang(name: &str) -> Option<Lang> {
    let n = name.to_ascii_lowercase();
    let l = |line_comments, block_comment, quotes, keywords, capitalized_types| Lang { line_comments, block_comment, quotes, keywords, capitalized_types };
    Some(match n.as_str() {
        "rust" | "rs" => l(C_LIKE_COMMENTS, Some(("/*", "*/")), &['"'], RUST, true),
        "swift" => l(C_LIKE_COMMENTS, Some(("/*", "*/")), &['"'], SWIFT, true),
        "js" | "jsx" | "javascript" | "ts" | "tsx" | "typescript" | "mjs" | "cjs" => l(C_LIKE_COMMENTS, Some(("/*", "*/")), &['"', '\'', '`'], JS, true),
        "go" | "golang" => l(C_LIKE_COMMENTS, Some(("/*", "*/")), &['"', '`'], GO, true),
        "c" | "h" | "cpp" | "c++" | "cc" | "hpp" | "objc" | "m" | "java" | "kotlin" | "kt" | "cs" | "csharp" | "scala" | "dart" => {
            l(C_LIKE_COMMENTS, Some(("/*", "*/")), &['"', '\''], C_FAMILY, true)
        }
        "python" | "py" => l(&["#"], None, &['"', '\''], PYTHON, true),
        "ruby" | "rb" => l(&["#"], None, &['"', '\''], RUBY, true),
        "sh" | "bash" | "zsh" | "shell" | "console" | "fish" => l(&["#"], None, &['"', '\''], SHELL, false),
        "sql" => l(&["--"], Some(("/*", "*/")), &['\''], SQL, false),
        "json" | "jsonc" => l(&["//"], None, &['"'], DATA, false),
        "yaml" | "yml" | "toml" | "ini" => l(&["#"], None, &['"', '\''], DATA, false),
        "css" | "scss" | "less" => l(&[], Some(("/*", "*/")), &['"', '\''], &[], false),
        "html" | "xml" | "svg" | "vue" => l(&[], Some(("<!--", "-->")), &['"', '\''], &[], false),
        _ => return None,
    })
}

/// A file's language from its name, for previews.
pub fn language_for_path(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name {
        "Dockerfile" | "Makefile" | "justfile" => return "sh".into(),
        _ => {}
    }
    name.rsplit_once('.').map(|(_, ext)| ext.to_owned()).unwrap_or_default()
}

/// Spans of `code` in `language` (empty: show it plain).
pub fn highlight(code: &str, language: &str) -> Vec<CodeSpan> {
    if code.len() > MAX_BYTES || code.is_empty() {
        return vec![];
    }
    if matches!(language.to_ascii_lowercase().as_str(), "diff" | "patch") {
        return diff_spans(code);
    }
    let Some(lang) = lang(language) else { return vec![] };
    let mut out: Vec<CodeSpan> = vec![];
    let push = |out: &mut Vec<CodeSpan>, text: &str, kind: &str| {
        if text.is_empty() {
            return;
        }
        match out.last_mut() {
            Some(last) if last.kind == kind => last.text.push_str(text),
            _ => out.push(CodeSpan { text: text.to_owned(), kind: kind.to_owned() }),
        }
    };
    let bytes = code.as_bytes();
    let mut i = 0;
    while i < code.len() {
        let rest = &code[i..];
        if let Some((open, close)) = lang.block_comment.filter(|(open, _)| rest.starts_with(open)) {
            let end = rest[open.len()..].find(close).map_or(rest.len(), |e| open.len() + e + close.len());
            push(&mut out, &rest[..end], "comment");
            i += end;
            continue;
        }
        if lang.line_comments.iter().any(|c| rest.starts_with(c)) {
            let end = rest.find('\n').unwrap_or(rest.len());
            push(&mut out, &rest[..end], "comment");
            i += end;
            continue;
        }
        let c = rest.chars().next().unwrap_or(' ');
        if lang.quotes.contains(&c) {
            let triple: String = std::iter::repeat_n(c, 3).collect();
            let (open_len, close): (usize, &str) = if rest.starts_with(&triple) { (3, &triple) } else { (1, &rest[..c.len_utf8()]) };
            let mut j = open_len;
            let mut end = rest.len();
            while j < rest.len() {
                let r = &rest[j..];
                if r.starts_with('\\') {
                    j += 1 + r[1..].chars().next().map_or(0, char::len_utf8);
                    continue;
                }
                if r.starts_with(close) {
                    end = j + close.len();
                    break;
                }
                // Single quotes and plain strings end at the line in most languages.
                if open_len == 1 && c != '`' && r.starts_with('\n') {
                    end = j;
                    break;
                }
                j += r.chars().next().map_or(1, char::len_utf8);
            }
            push(&mut out, &rest[..end], "string");
            i += end;
            continue;
        }
        let prev_ident = i > 0 && is_ident(bytes[i - 1] as char);
        if c.is_ascii_digit() && !prev_ident {
            let end = rest.find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '.')).unwrap_or(rest.len());
            push(&mut out, &rest[..end], "number");
            i += end;
            continue;
        }
        if (is_ident_start(c) || (c == '@' && rest.len() > 1)) && !prev_ident {
            let start = if c == '@' { 1 } else { 0 };
            let end = start + rest[start..].find(|ch: char| !is_ident(ch)).unwrap_or(rest.len() - start);
            let word = &rest[..end];
            let kind = if lang.keywords.contains(&word) || (lang.keywords == SQL && SQL.contains(&word.to_ascii_lowercase().as_str())) {
                "keyword"
            } else if rest[end..].starts_with('(') && c != '@' {
                "function"
            } else if lang.capitalized_types && word.chars().next().is_some_and(char::is_uppercase) && word.len() > 1 {
                "type"
            } else {
                "plain"
            };
            push(&mut out, word, kind);
            i += end.max(1);
            continue;
        }
        let len = c.len_utf8();
        push(&mut out, &rest[..len], "plain");
        i += len;
    }
    out
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$'
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

fn diff_spans(code: &str) -> Vec<CodeSpan> {
    code.split_inclusive('\n')
        .map(|line| {
            let kind = match line.as_bytes().first() {
                Some(b'+') if !line.starts_with("+++") => "added",
                Some(b'-') if !line.starts_with("---") => "removed",
                Some(b'@') => "keyword",
                _ => "plain",
            };
            CodeSpan { text: line.to_owned(), kind: kind.into() }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(code: &str, lang: &str) -> Vec<(String, String)> {
        highlight(code, lang).into_iter().filter(|s| s.kind != "plain").map(|s| (s.kind, s.text)).collect()
    }

    #[test]
    fn spans_cover_the_code_exactly() {
        let code = "fn main() {\n    let s = \"a \\\"q\\\" ż\"; // note\n    println!(\"{}\", 42);\n}\n";
        let joined: String = highlight(code, "rust").into_iter().map(|s| s.text).collect();
        assert_eq!(joined, code);
    }

    #[test]
    fn rust_keywords_strings_comments_numbers_types_and_calls() {
        let k = kinds("let x: Vec<u8> = parse(\"hi\", 0x1f); // done\n/* block */", "rust");
        assert!(k.contains(&("keyword".into(), "let".into())));
        assert!(k.contains(&("type".into(), "Vec".into())));
        assert!(k.contains(&("function".into(), "parse".into())));
        assert!(k.contains(&("string".into(), "\"hi\"".into())));
        assert!(k.contains(&("number".into(), "0x1f".into())));
        assert!(k.contains(&("comment".into(), "// done".into())));
        assert!(k.contains(&("comment".into(), "/* block */".into())));
    }

    #[test]
    fn python_triple_quotes_and_hash_comments() {
        let k = kinds("def f():\n    \"\"\"doc\nmore\"\"\"\n    return None  # nothing\n", "py");
        assert!(k.contains(&("keyword".into(), "def".into())));
        assert!(k.contains(&("string".into(), "\"\"\"doc\nmore\"\"\"".into())));
        assert!(k.contains(&("comment".into(), "# nothing".into())));
    }

    #[test]
    fn unknown_languages_and_huge_code_stay_plain() {
        assert!(highlight("anything", "brainfuck").is_empty());
        assert!(highlight(&"x".repeat(MAX_BYTES + 1), "rust").is_empty());
        assert!(highlight("ident2 = 3", "python").iter().all(|s| s.text != "2"), "digits inside a name aren't numbers");
    }

    #[test]
    fn diffs_mark_added_and_removed_lines() {
        let k = kinds("@@ -1 +1 @@\n-old\n+new\n same\n", "diff");
        assert_eq!(k, [("keyword".into(), "@@ -1 +1 @@\n".into()), ("removed".into(), "-old\n".into()), ("added".into(), "+new\n".into())]);
        assert_eq!(language_for_path("src/main.rs"), "rs");
        assert_eq!(language_for_path("a/Makefile"), "sh");
    }
}

//! Mechanical output repair for evidence-validation drift (wrong line numbers,
//! diff side, or slightly truncated quotes).
//!
//! Enforces strict semantic invariance via [`preserves_report`]: a repair pass
//! may ONLY correct `line`, `side`, `quote`, and previously-empty `counterEvidence`.
//! If it adds, drops, or rewrites any finding's title, detail, action, or severity,
//! the repair is rejected.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "structural JSON invariance comparison across model outputs")]

use serde_json::{Value, json};

fn normalize_for_identity(mut value: Value) -> Value {
    if let Some(obj) = value.as_object_mut() {
        for field in ["findings", "suggestions", "questions"] {
            obj.entry(field).or_insert_with(|| json!([]));
        }
        // Summary and reviewedFiles don't change finding identity, keep summary intact.
    }
    for field in ["findings", "suggestions", "questions"] {
        if let Some(items) = value.get_mut(field).and_then(Value::as_array_mut) {
            for item in items {
                if let Some(obj) = item.as_object_mut() {
                    obj.remove("line");
                    obj.remove("side");
                }
                for ev_key in ["evidence", "counterEvidence"] {
                    if let Some(refs) = item.get_mut(ev_key).and_then(Value::as_array_mut) {
                        for r in refs {
                            if let Some(robj) = r.as_object_mut() {
                                robj.remove("line");
                                robj.remove("quote");
                            }
                        }
                    }
                }
            }
        }
    }
    value
}

/// Returns `true` iff `repaired` preserves every finding, suggestion, and question
/// from `original` without altering semantic fields (only line/side/quote/empty counterEvidence).
pub fn preserves_report(original: &Value, repaired: &Value) -> bool {
    let before = normalize_for_identity(original.clone());
    let mut after = normalize_for_identity(repaired.clone());
    for field in ["findings", "suggestions", "questions"] {
        if let (Some(b_arr), Some(a_arr)) = (before.get(field).and_then(Value::as_array), after.get_mut(field).and_then(Value::as_array_mut)) {
            if b_arr.len() != a_arr.len() {
                return false;
            }
            for (b, a) in b_arr.iter().zip(a_arr.iter_mut()) {
                if b.get("counterEvidence").and_then(Value::as_array).is_some_and(Vec::is_empty)
                    && let Some(cev) = a.get_mut("counterEvidence").and_then(Value::as_array_mut)
                {
                    cev.clear();
                }
            }
        }
    }
    before == after
}

/// Applies ONLY mechanical corrections (`line`, `side`, `evidence[].line/quote`,
/// and `counterEvidence`) from `repaired` onto `original` when `preserves_report` holds.
pub fn apply_mechanical_fixes(original: &Value, repaired: &Value) -> Option<Value> {
    if !preserves_report(original, repaired) {
        return None;
    }
    let mut merged = original.clone();
    for field in ["findings", "suggestions", "questions"] {
        let (Some(m_arr), Some(r_arr)) = (merged.get_mut(field).and_then(Value::as_array_mut), repaired.get(field).and_then(Value::as_array)) else {
            continue;
        };
        for (m, r) in m_arr.iter_mut().zip(r_arr.iter()) {
            let (Some(m_obj), Some(r_obj)) = (m.as_object_mut(), r.as_object()) else { continue };
            if let Some(line) = r_obj.get("line") {
                m_obj.insert("line".into(), line.clone());
            }
            if let Some(side) = r_obj.get("side") {
                m_obj.insert("side".into(), side.clone());
            }
            for ev_key in ["evidence", "counterEvidence"] {
                if let Some(r_refs) = r_obj.get(ev_key) {
                    m_obj.insert(ev_key.into(), r_refs.clone());
                }
            }
        }
    }
    Some(merged)
}

/// Builds a targeted prompt asking the model to fix only line numbers, diff sides, or quotes.
pub fn repair_prompt(original_json: &str, validation_error: &str) -> String {
    format!(
        "Your previous structured review JSON failed deterministic evidence validation:\n\
         Validation error: {validation_error}\n\n\
         Fix ONLY the broken `line`, `side`, or verbatim `quote` fields (or populate missing `counterEvidence`) \
         using the exact files in the read-only snapshot directory (`CHANGED/`, `BASE/`, `RELATED/`, `CONVENTIONS/`).\n\
         DO NOT add, remove, reorder, or rewrite any finding, suggestion, or question's `title`, `detail`, `action`, `severity`, or `path`.\n\n\
         Original JSON:\n{original_json}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_line_and_quote_fixes_but_rejects_semantic_edits() {
        let orig = json!({
            "summary": "s",
            "findings": [{
                "title": "Bug", "detail": "d", "action": "a", "severity": "high",
                "path": "a.rs", "line": 99, "side": "new",
                "evidence": [{ "path": "a.rs", "line": 99, "quote": "bad" }],
                "counterEvidence": []
            }],
            "suggestions": [],
            "questions": [],
            "reviewedFiles": ["a.rs"]
        });
        let fixed = json!({
            "summary": "s",
            "findings": [{
                "title": "Bug", "detail": "d", "action": "a", "severity": "high",
                "path": "a.rs", "line": 12, "side": "new",
                "evidence": [{ "path": "a.rs", "line": 12, "quote": "exact()" }],
                "counterEvidence": []
            }],
            "suggestions": [],
            "questions": [],
            "reviewedFiles": ["a.rs"]
        });
        assert!(preserves_report(&orig, &fixed));
        let applied = apply_mechanical_fixes(&orig, &fixed).unwrap();
        assert_eq!(applied["findings"][0]["line"], 12);
        assert_eq!(applied["findings"][0]["evidence"][0]["quote"], "exact()");

        // Dropping a finding or changing its title MUST be rejected.
        let altered = json!({
            "summary": "s",
            "findings": [{
                "title": "Different bug", "detail": "d", "action": "a", "severity": "high",
                "path": "a.rs", "line": 12, "side": "new",
                "evidence": [{ "path": "a.rs", "line": 12, "quote": "exact()" }],
                "counterEvidence": []
            }],
            "suggestions": [],
            "questions": [],
            "reviewedFiles": ["a.rs"]
        });
        assert!(!preserves_report(&orig, &altered));
        assert!(apply_mechanical_fixes(&orig, &altered).is_none());
    }
}

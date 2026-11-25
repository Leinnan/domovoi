//! The hidden `__document-text` subcommand the server extracts attachment
//! text with (`document_text.rs`): the real binary, on the fixture documents
//! in `tests/fixtures/documents/`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/documents").join(name)
}

/// The worker's exit code and stdout for a document.
fn extract(name: &str, extension: &str) -> (i32, String) {
    // A home of its own: the worker must not need (or touch) one.
    let home = std::env::temp_dir().join(format!("domovoi-doctext-home-{}", std::process::id()));
    let out = Command::new(env!("CARGO_BIN_EXE_domovoi-server"))
        .args(["--home-dir", &home.to_string_lossy(), "__document-text"])
        .arg(fixture(name))
        .arg(extension)
        .output()
        .unwrap();
    assert!(!home.exists(), "the worker made a home directory");
    (out.status.code().unwrap_or(-1), String::from_utf8(out.stdout).unwrap())
}

#[test]
fn extracts_each_kind_of_document() {
    let (code, text) = extract("report.docx", ".docx");
    assert_eq!((code, text.as_str()), (0, "# Quarterly report\nRevenue grew\tby 12%.\nRegion\tSales\nNorth\t1 200\nSouth\t900\nThe end."));
    let (code, text) = extract("deck.pptx", ".pptx");
    assert_eq!(code, 0);
    assert!(text.starts_with("## Slide 1\nWelcome") && text.contains("Notes:\nSay hello first"), "{text:?}");
    let (code, text) = extract("budget.xlsx", ".xlsx");
    assert_eq!(code, 0);
    assert!(text.starts_with("## Sheet: Budget\nItem\tCost\tPaid\n"), "{text:?}");
    let (code, text) = extract("letter.pdf", ".pdf");
    assert_eq!(code, 0);
    assert!(text.contains("Hello from a PDF"), "{text:?}");
}

#[test]
fn rejects_malformed_documents_with_exit_2() {
    assert_eq!(extract("broken.docx", ".docx"), (2, String::new()));
    assert_eq!(extract("broken.pdf", ".pdf"), (2, String::new()));
    assert_eq!(extract("letter.pdf", ".exe").0, 2);
    // A zip bomb stops at the expansion limit with a note, not a crash.
    let (code, text) = extract("bomb.docx", ".docx");
    assert_eq!(code, 0);
    assert!(text.ends_with("[Extracted text truncated at document extraction limits.]"), "{text:?}");
}

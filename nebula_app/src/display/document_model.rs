//! File-type routing shared by the legacy and GPUI document tabs.

use std::path::Path;

/// Extensions opened by the read-only document viewer.
pub fn viewable_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("md" | "markdown" | "json" | "jsonl" | "ndjson" | "txt" | "log")
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::viewable_file;

    #[test]
    fn recognizes_document_extensions_case_insensitively() {
        assert!(viewable_file(Path::new("README.MD")));
        assert!(viewable_file(Path::new("events.jsonl")));
        assert!(viewable_file(Path::new("service.log")));
        assert!(!viewable_file(Path::new("archive.zip")));
    }
}

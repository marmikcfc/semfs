//! Legacy OLE2 PowerPoint (`.ppt`) — KNOWN GAP, deferred to a fast-follow.
//!
//! Evaluated `litchi` 0.0.1 (`ole` feature): it compiles pure-Rust and does not
//! panic, but on the real chanpin `.ppt` it returns only `"*"` — the actual CJK
//! slide text is not recovered. Indexing that junk is worse than not indexing
//! (it pollutes search), and it isn't worth pulling an alpha crate into the
//! production binary for **one file** in the corpus. So `.ppt` is routed to the
//! unindexed bucket (accounted, never silently dropped). A proper extractor
//! (LibreOffice headless, or a maturer OLE2 PowerPoint decoder) is a fast-follow
//! if `.ppt` ever appears at scale in another persona.

/// Legacy `.ppt` extraction is not implemented — always `None` (known gap).
pub fn extract_ppt(_bytes: &[u8]) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const PPT: &[u8] = include_bytes!("../../tests/fixtures/chanpin/sample.ppt");

    #[test]
    fn legacy_ppt_is_a_known_gap_returns_none() {
        // Until a real OLE2 PowerPoint decoder lands, `.ppt` is unindexed, not
        // crashed and not silently dropped.
        assert_eq!(extract_ppt(PPT), None);
    }
}

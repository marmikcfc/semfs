//! PDF text extraction via `pdf-extract` (pure-Rust). Only the text layer is
//! recovered; scanned/image-only PDFs yield nothing and are routed to the
//! unindexed bucket by the caller (never silently dropped).

use std::cell::Cell;
use std::sync::Once;

thread_local! {
    /// When set, the installed panic hook stays silent for the current thread —
    /// used to mute pdf-extract's expected panics (e.g. CJK CID fonts) which we
    /// already contain. Per-thread, so it never hides panics elsewhere.
    static MUTE_PANIC: Cell<bool> = const { Cell::new(false) };
}

static HOOK_INIT: Once = Once::new();

/// Install (once) a panic hook that defers to the previous hook EXCEPT on
/// threads that have opted into muting. Other panics print exactly as before.
fn install_quiet_hook() {
    HOOK_INIT.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !MUTE_PANIC.with(Cell::get) {
                previous(info);
            }
        }));
    });
}

/// Extract the text layer of a PDF. `None` if it can't be parsed or has no text.
pub fn extract_pdf(bytes: &[u8]) -> Option<String> {
    // pdf-extract can panic on some malformed/CID-font PDFs; contain it so a
    // single bad file never aborts an import, and mute the hook so a known-bad
    // corpus (e.g. CJK PDFs) doesn't flood stderr with backtraces.
    install_quiet_hook();
    MUTE_PANIC.with(|m| m.set(true));
    let caught = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(bytes));
    MUTE_PANIC.with(|m| m.set(false));

    let text = caught.ok()?.ok()?;
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Latin text-layer PDF — the mechanism's happy path.
    const ASCII_PDF: &[u8] = include_bytes!("../../tests/fixtures/ascii_textlayer.pdf");
    // Real chanpin PDF: CJK CID font with a non-Identity-H CMap, which
    // pdf-extract 0.7 cannot decode (it panics). Documents the gap.
    const CJK_PDF: &[u8] = include_bytes!("../../tests/fixtures/chanpin/sample.pdf");

    #[test]
    fn extracts_latin_text_layer() {
        let t = extract_pdf(ASCII_PDF).expect("latin pdf should extract");
        assert!(
            t.contains("invoice total 4200"),
            "missing known pdf text; got: {}",
            &t.chars().take(120).collect::<String>()
        );
    }

    #[test]
    fn cjk_cid_pdf_is_a_contained_gap_not_a_crash() {
        // pdf-extract panics on this PDF's CMap; the extractor must swallow that
        // and return None so the file is accounted as unindexed, never aborting
        // the import. KNOWN GAP: CJK PDFs need a follow-up (OCR or CID decoder).
        assert_eq!(extract_pdf(CJK_PDF), None);
    }

    #[test]
    fn garbage_returns_none_no_panic() {
        assert_eq!(extract_pdf(&[0x25, 0x50, 0x44, 0x46, 0x00, 0x01]), None);
    }
}

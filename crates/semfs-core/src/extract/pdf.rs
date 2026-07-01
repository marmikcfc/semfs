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

/// Extract a PDF's text layer with poppler's `pdftotext` — the CJK-capable
/// fallback for the many real PDFs whose CID fonts `pdf-extract` can't decode
/// (it panics, handled above). Fast and LOCAL (no OCR, no network), so it
/// replaces the slow `mistral-ocr` path for the common case of a large CJK PDF
/// that simply has a text layer. Returns `None` if pdftotext is absent, errors,
/// or the PDF has no text layer (a true scan — the caller then escalates to OCR).
pub fn pdftotext(bytes: &[u8]) -> Option<String> {
    let dir = tempfile::tempdir().ok()?;
    let input = dir.path().join("in.pdf");
    std::fs::write(&input, bytes).ok()?;
    let out = std::process::Command::new("pdftotext")
        .arg("-q") // suppress diagnostics
        .arg(&input)
        .arg("-") // write extracted text to stdout
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
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

    /// Live (skips without poppler): the CJK CID-font PDF that `extract_pdf`
    /// returns `None` for must extract via `pdftotext`. This is the fallback that
    /// closes the CJK-PDF gap without the slow/timeout-prone OCR path. Validated
    /// on the seed box (poppler installed); skipped in CI.
    #[test]
    fn pdftotext_recovers_cjk_text_layer_when_available() {
        let have_poppler = std::process::Command::new("pdftotext")
            .arg("-v")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !have_poppler {
            eprintln!("skipping: pdftotext (poppler) not installed");
            return;
        }
        let t = pdftotext(CJK_PDF).expect("pdftotext should read the CJK text layer");
        assert!(
            t.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "expected CJK characters from the text layer, got: {:?}",
            t.chars().take(60).collect::<String>()
        );
    }
}

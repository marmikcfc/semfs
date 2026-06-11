//! Local document text extraction (L1 parse).
//!
//! Routes raw file bytes to a per-format extractor so binary documents
//! (Office/PDF/legacy/images) can feed the local L1–L7 index without a cloud
//! round-trip. See `tickets/local-document-extractors/issue.md`.
//!
//! Routing is by **content sniffing (magic bytes)**, not by file extension:
//! the real `chanpin` corpus mislabels ~20% of `.xlsx` files (PDFs, HTML error
//! pages, legacy `.xls`, even a `.docx`, all wearing an `.xlsx` name), so the
//! extension is not a reliable type signal. We open the box and look.

pub mod legacy_ppt;
pub mod ocr;
pub mod ooxml;
pub mod pdf;
pub mod spreadsheet;
pub mod summary;

/// Per-file cap on extracted text. A retrieval index needs the document's head,
/// not every cell of a 23 MB spreadsheet: beyond this we truncate, because the
/// downstream `index()` chunks + embeds the WHOLE returned string in one shot, so
/// an uncapped blob drives unbounded chunk count → tens of minutes of embedding
/// and multi-GB RSS, stalling a seed on a single file (RCA
/// 2026-06-03-extract-unbounded-large-doc-hang).
const MAX_EXTRACT_BYTES: usize = 1024 * 1024; // 1 MiB

/// Wall-clock budget for a single extractor. Defends against a parser that
/// CPU-loops on a pathological file (e.g. `pdf-extract` on some PDFs) — the size
/// cap can't, since it only applies AFTER extraction returns. On timeout the file
/// is routed to the unindexed bucket so the import keeps moving.
const EXTRACT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

/// Wall-clock budget for a vision/OCR call (image or PDF fallback). Larger than
/// `EXTRACT_TIMEOUT`: gpt-4.1-mini processing a multi-page PDF natively is slower
/// than a CPU parse. On timeout → unindexed (the detached request is abandoned).
const OCR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Wall-clock budget for spreadsheet extraction WITH per-sheet LLM summaries: a
/// cheap parse plus one gpt-4.1-mini call per sheet (cache-served on re-seeds).
/// Generous because a multi-sheet workbook fans out several network calls. On
/// timeout → unindexed; note `summarize_workbook` already degrades each sheet to
/// raw cells on a per-call failure, so only a true hang reaches this ceiling.
const SUMMARY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// Wall-clock budget for a headless LibreOffice conversion — the universal
/// fallback for legacy OLE `.doc`/`.ppt` and any office binary the pure-Rust
/// extractors can't read (the same tool Workspace-Bench agents shell out to).
/// Generous because soffice cold-starts an office process; bounded so one slow
/// file can't stall the import.
const SOFFICE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// Wall-clock budget for page-split PDF OCR (`pdftoppm` rasterize + per-page
/// vision, bounded-parallel). Larger than the single-shot `OCR_TIMEOUT` because
/// it fans out up to `MAX_OCR_PAGES` small requests — but each page is tiny, so
/// the bounded-concurrency fan-out still finishes well inside this.
const PAGED_OCR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Wall-clock budget for the VLM-describe fallback on an OFFICE file: a soffice
/// convert-to-PDF, then `pdftoppm` rasterize, then a few describe calls. Sum of
/// the soffice + paged-render budgets, bounded so one file can't stall a seed.
const DESCRIBE_OFFICE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(240);

/// Extract searchable text from a document's raw bytes, routing by sniffed
/// format. CPU parsers run on the blocking pool (time-bounded); image OCR is a
/// key-gated network call. The returned text is capped to `MAX_EXTRACT_BYTES`.
/// Returns `None` when the file has no recoverable text (the caller accounts it
/// as unindexed — it is never silently dropped). A single file's failure never
/// panics, hangs, or aborts the import.
pub async fn extract_text(filepath: &str, bytes: &[u8]) -> Option<String> {
    let fmt = sniff(bytes);
    let owned = bytes.to_vec();
    // Opt-in merged vision tier: for visual formats it transcribes text if
    // present, else describes — so a textless image is never indexed as OCR junk.
    let describe = vlm_describe_enabled();
    let primary = match fmt {
        DocFormat::Docx => blocking(move || ooxml::extract_docx(&owned), EXTRACT_TIMEOUT).await,
        DocFormat::Pptx => blocking(move || ooxml::extract_pptx(&owned), EXTRACT_TIMEOUT).await,
        DocFormat::Xlsx | DocFormat::Xls => {
            // Per-sheet LLM summaries make numeric tables findable (bare cells
            // embed to nothing). Key-gated: no key ⇒ summarize_workbook falls
            // back to flattened raw cells, so this never regresses the offline
            // path. See tickets/summary-augmented-table-retrieval/.
            let fp = filepath.to_string();
            blocking(
                move || summary::summarize_workbook(&fp, &spreadsheet::extract_sheets(&owned)),
                SUMMARY_TIMEOUT,
            )
            .await
        }
        DocFormat::Pdf => {
            // Three tiers, cheapest first: (1) pure-Rust text layer; (2) poppler
            // `pdftotext` — reads CJK CID-font text layers that pdf-extract can't
            // decode, fast + local, so a large CJK PDF never reaches the slow OCR;
            // (3) gpt-4.1-mini `mistral-ocr` for TRUE scans only (no text layer).
            // OCR is the last resort, not the second — it timed out on multi-MB
            // CJK PDFs that actually had extractable text (local-seed-coverage-gaps).
            let for_poppler = owned.clone();
            let for_paged = owned.clone();
            let for_ocr = owned.clone();
            match blocking(move || pdf::extract_pdf(&owned), EXTRACT_TIMEOUT).await {
                Some(t) => Some(t),
                None => match blocking(move || pdf::pdftotext(&for_poppler), EXTRACT_TIMEOUT).await
                {
                    Some(t) => Some(t),
                    // Image-only scan (no text layer): merged vision
                    // (transcribe-or-describe) when enabled, else the
                    // transcribe-only OCR chain (page-split, then whole-PDF).
                    None => {
                        if describe {
                            blocking(
                                move || ocr::vision_extract_pdf_paged(&for_paged),
                                PAGED_OCR_TIMEOUT,
                            )
                            .await
                        } else {
                            match blocking(move || ocr::ocr_pdf_paged(&for_paged), PAGED_OCR_TIMEOUT)
                                .await
                            {
                                Some(t) => Some(t),
                                None => blocking(move || ocr::ocr_pdf(&for_ocr), OCR_TIMEOUT).await,
                            }
                        }
                    }
                },
            }
        }
        DocFormat::Jpeg => {
            if describe {
                blocking(move || ocr::vision_extract_image(&owned), OCR_TIMEOUT).await
            } else {
                blocking(move || ocr::ocr_image(&owned), OCR_TIMEOUT).await
            }
        }
        // Known gaps / non-document content: accounted as unindexed by the caller.
        DocFormat::Ppt => legacy_ppt::extract_ppt(bytes),
        DocFormat::Html | DocFormat::Unknown => None,
    };
    // LibreOffice fallback — convert legacy OLE (`.doc`/`.ppt`) and any office
    // binary the pure-Rust path couldn't read, the same tool Workspace-Bench
    // agents shell out to. Gated to real office containers (OLE2 / OOXML zip) so
    // soffice never fires on HTML or garbage; a no-op where soffice is absent
    // (the spawn errors → None). RCA: tickets/local-seed-coverage-gaps (legacy).
    let result = match primary {
        Some(t) => Some(t),
        None if is_office_binary(bytes) => {
            let owned2 = bytes.to_vec();
            let ext = soffice_ext(fmt);
            blocking(move || soffice_to_text(&owned2, ext), SOFFICE_TIMEOUT).await
        }
        None => None,
    };
    // Office VLM fallback (opt-in): image-only slides/sheets that yielded no text
    // get a merged vision pass (transcribe-or-describe) via soffice→PDF→render.
    // Images/PDFs are already handled by the merged vision step in `primary`.
    let result = match result {
        Some(t) => Some(t),
        None if describe && is_office_binary(bytes) => {
            let b = bytes.to_vec();
            let ext = soffice_ext(fmt);
            blocking(
                move || soffice_to_pdf(&b, ext).and_then(|pdf| ocr::vision_extract_pdf_paged(&pdf)),
                DESCRIBE_OFFICE_TIMEOUT,
            )
            .await
        }
        None => None,
    };
    let result = result.map(cap_text);
    if result.is_none() {
        tracing::debug!(filepath, ?fmt, "extract_text produced no text (unindexed)");
    }
    result
}

/// Raw flattened-cell text for a spreadsheet, for materializing the
/// `.extracted.md` sibling. With summary-augmented indexing, `extract_text`
/// returns a per-sheet *summary* for xlsx so retrieval embeds prose, not
/// number-noise — but the agent must read the actual rows to compute answers.
/// So the sibling holds the raw table, not the summary (summary FINDS, table
/// ANSWERS). The join mirrors the no-key raw-cell representation, so a
/// summary-seed sibling reads identically to a raw-seed sibling.
///
/// Returns `None` for non-spreadsheet bytes; the caller then falls back to the
/// indexed text, which for those formats already IS the real extracted text.
pub fn raw_table_for_sibling(bytes: &[u8]) -> Option<String> {
    match sniff(bytes) {
        DocFormat::Xlsx | DocFormat::Xls => {
            let sheets = spreadsheet::extract_sheets(bytes);
            if sheets.is_empty() {
                return None;
            }
            Some(
                sheets
                    .iter()
                    .map(|s| s.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        }
        _ => None,
    }
}

/// Whether the opt-in VLM-describe fallback is enabled (`SEMFS_VLM_DESCRIBE`).
/// Off by default: it adds vision-API cost and indexes descriptions rather than
/// source text, so it's a deliberate per-run choice.
fn vlm_describe_enabled() -> bool {
    std::env::var("SEMFS_VLM_DESCRIBE")
        .map(|v| matches!(v.trim(), "1" | "on" | "true" | "yes"))
        .unwrap_or(false)
}

/// Convert an office document to PDF bytes with headless LibreOffice (so the
/// describe tier can rasterize image-only slides/sheets). Mirrors
/// [`soffice_to_text`]'s private-profile pattern. `None` if soffice is absent,
/// errors, or produces no PDF.
fn soffice_to_pdf(bytes: &[u8], ext: &str) -> Option<Vec<u8>> {
    let dir = tempfile::tempdir().ok()?;
    let input = dir.path().join(format!("in.{ext}"));
    std::fs::write(&input, bytes).ok()?;
    let profile = dir.path().join("profile");
    let status = std::process::Command::new("soffice")
        .args(["--headless", "--norestore", "--nolockcheck"])
        .arg(format!("-env:UserInstallation=file://{}", profile.display()))
        .args(["--convert-to", "pdf", "--outdir"])
        .arg(dir.path())
        .arg(&input)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    std::fs::read(dir.path().join("in.pdf"))
        .ok()
        .filter(|b| !b.is_empty())
}

/// Truncate to at most `MAX_EXTRACT_BYTES`, on a UTF-8 char boundary so the
/// result stays valid (and `String::truncate` never panics). Logs when it bites.
fn cap_text(mut s: String) -> String {
    if s.len() <= MAX_EXTRACT_BYTES {
        return s;
    }
    let mut end = MAX_EXTRACT_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    tracing::warn!(
        capped_bytes = end,
        "extracted text exceeded cap; indexing the head only (partial)"
    );
    s
}

/// Run a CPU-bound extractor on the blocking pool with a wall-clock budget.
/// Flattens a `JoinError` (e.g. a panic that escaped an extractor) to `None` so
/// it can't abort import; on timeout returns `None` (the slow `spawn_blocking`
/// thread can't be cancelled, so it runs to completion detached — the lesser
/// evil versus stalling the whole import behind one pathological file).
async fn blocking<F>(f: F, budget: std::time::Duration) -> Option<String>
where
    F: FnOnce() -> Option<String> + Send + 'static,
{
    let handle = tokio::task::spawn_blocking(f);
    match tokio::time::timeout(budget, handle).await {
        Ok(joined) => joined.ok().flatten(),
        Err(_elapsed) => {
            tracing::warn!("extractor exceeded time budget; routing to unindexed");
            None
        }
    }
}

/// True if the bytes are a real office-binary container — legacy OLE2
/// (`D0CF11E0`) or an OOXML zip (`PK`). The gate for the LibreOffice fallback,
/// so soffice never runs on HTML, images, or arbitrary garbage.
fn is_office_binary(b: &[u8]) -> bool {
    b.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]) || b.starts_with(b"PK\x03\x04")
}

/// Extension to hand LibreOffice so it selects the right import filter. An OLE2
/// file that sniffed as `Unknown` is almost always legacy Word, so default to
/// `.doc`.
fn soffice_ext(fmt: DocFormat) -> &'static str {
    match fmt {
        DocFormat::Docx => "docx",
        DocFormat::Xlsx => "xlsx",
        DocFormat::Pptx => "pptx",
        DocFormat::Xls => "xls",
        DocFormat::Ppt => "ppt",
        _ => "doc",
    }
}

/// Convert a document to UTF-8 text with headless LibreOffice. Each call uses a
/// PRIVATE `UserInstallation` profile dir so concurrent conversions during a warm
/// can't collide on the shared default profile (soffice refuses a second instance
/// otherwise). Returns `None` when soffice is absent, errors, or yields no text —
/// the caller then accounts the file unindexed (never silently dropped).
fn soffice_to_text(bytes: &[u8], ext: &str) -> Option<String> {
    // LibreOffice's text-export filter is module-specific: Writer exports plain
    // text (`txt:Text`), Calc exports CSV, Impress has no rich text export so we
    // take its best-effort `txt`. The output filename's extension follows.
    let (filter, out_ext) = match ext {
        "xls" | "xlsx" => ("csv", "csv"),
        "ppt" | "pptx" => ("txt", "txt"),
        _ => ("txt:Text", "txt"), // doc/docx (Writer) + OLE-Unknown (legacy .doc)
    };
    let dir = tempfile::tempdir().ok()?;
    let input = dir.path().join(format!("in.{ext}"));
    std::fs::write(&input, bytes).ok()?;
    let profile = dir.path().join("profile");
    let status = std::process::Command::new("soffice")
        .args(["--headless", "--norestore", "--nolockcheck"])
        .arg(format!("-env:UserInstallation=file://{}", profile.display()))
        .args(["--convert-to", filter, "--outdir"])
        .arg(dir.path())
        .arg(&input)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let out = dir.path().join(format!("in.{out_ext}"));
    let text = std::fs::read_to_string(out).ok()?;
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

/// The true format of a file, determined from its leading bytes (and, for
/// container formats, a cheap peek at internal markers). This is the routing
/// key for picking an extractor — the chosen parser still confirms the format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocFormat {
    /// OOXML Word (`PK` zip containing `word/`).
    Docx,
    /// OOXML Excel (`PK` zip containing `xl/`).
    Xlsx,
    /// OOXML PowerPoint (`PK` zip containing `ppt/`).
    Pptx,
    /// PDF (`%PDF`).
    Pdf,
    /// Legacy OLE2 Excel (`D0CF11E0` with a `Workbook` stream).
    Xls,
    /// Legacy OLE2 PowerPoint (`D0CF11E0` with a `PowerPoint Document` stream).
    Ppt,
    /// JPEG image (`FFD8FF`) — an OCR target.
    Jpeg,
    /// HTML (`<html`/`<!DOCTYPE`) — often a saved web/error page mislabeled.
    Html,
    /// Unrecognized — caller records it as unindexed, never silently drops it.
    Unknown,
}

/// Identify a file's true format from its raw bytes.
pub fn sniff(bytes: &[u8]) -> DocFormat {
    if bytes.starts_with(b"%PDF") {
        return DocFormat::Pdf;
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return DocFormat::Jpeg;
    }
    // OOXML: a zip whose cleartext local-header filenames name the type dir.
    if bytes.starts_with(b"PK\x03\x04") {
        if contains(bytes, b"word/document.xml") {
            return DocFormat::Docx;
        }
        if contains(bytes, b"xl/workbook.xml") {
            return DocFormat::Xlsx;
        }
        if contains(bytes, b"ppt/presentation.xml") {
            return DocFormat::Pptx;
        }
        return DocFormat::Unknown;
    }
    // Legacy OLE2 compound file: stream names are stored UTF-16LE.
    if bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]) {
        if contains(bytes, &utf16le("Workbook")) {
            return DocFormat::Xls;
        }
        if contains(bytes, &utf16le("PowerPoint Document")) {
            return DocFormat::Ppt;
        }
        return DocFormat::Unknown;
    }
    if leading_tag_is(bytes, b"<html") || leading_tag_is(bytes, b"<!doctype") {
        return DocFormat::Html;
    }
    DocFormat::Unknown
}

/// True if `needle` occurs anywhere in `haystack`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// ASCII string as UTF-16LE bytes (for OLE2 stream-name matching).
fn utf16le(s: &str) -> Vec<u8> {
    s.bytes().flat_map(|b| [b, 0]).collect()
}

/// True if `bytes`, after leading ASCII whitespace, begins with `tag`
/// (case-insensitive on `tag`).
fn leading_tag_is(bytes: &[u8], tag: &[u8]) -> bool {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let rest = &bytes[start..];
    rest.len() >= tag.len() && rest[..tag.len()].eq_ignore_ascii_case(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real chanpin-corpus samples (see tests/fixtures/chanpin/MANIFEST.md).
    const DOCX: &[u8] = include_bytes!("../../tests/fixtures/chanpin/sample.docx");
    const XLSX: &[u8] = include_bytes!("../../tests/fixtures/chanpin/sample.xlsx");
    const PPTX: &[u8] = include_bytes!("../../tests/fixtures/chanpin/sample.pptx");
    const PDF: &[u8] = include_bytes!("../../tests/fixtures/chanpin/sample.pdf");
    const XLS: &[u8] = include_bytes!("../../tests/fixtures/chanpin/sample.xls");
    const PPT: &[u8] = include_bytes!("../../tests/fixtures/chanpin/sample.ppt");
    const JPG: &[u8] = include_bytes!("../../tests/fixtures/chanpin/sample.jpg");
    const HTML_AS_XLSX: &[u8] =
        include_bytes!("../../tests/fixtures/chanpin/edge_html_masquerading_as.xlsx");

    #[test]
    fn sniffs_pdf_by_magic() {
        assert_eq!(sniff(PDF), DocFormat::Pdf);
    }

    #[test]
    fn sniffs_jpeg_by_magic() {
        assert_eq!(sniff(JPG), DocFormat::Jpeg);
    }

    #[test]
    fn sniffs_docx_by_zip_word_marker() {
        assert_eq!(sniff(DOCX), DocFormat::Docx);
    }

    #[test]
    fn sniffs_xlsx_by_zip_xl_marker() {
        assert_eq!(sniff(XLSX), DocFormat::Xlsx);
    }

    #[test]
    fn sniffs_pptx_by_zip_ppt_marker() {
        assert_eq!(sniff(PPTX), DocFormat::Pptx);
    }

    #[test]
    fn sniffs_legacy_xls_by_ole2_stream() {
        assert_eq!(sniff(XLS), DocFormat::Xls);
    }

    #[test]
    fn sniffs_legacy_ppt_by_ole2_stream() {
        assert_eq!(sniff(PPT), DocFormat::Ppt);
    }

    #[test]
    fn office_binary_gate_admits_ole_and_ooxml_only() {
        // The LibreOffice fallback fires ONLY for real office containers, so it
        // never wastes a soffice spawn on HTML/images/garbage.
        assert!(is_office_binary(PPT)); // legacy OLE2
        assert!(is_office_binary(XLS)); // legacy OLE2
        assert!(is_office_binary(DOCX)); // OOXML zip
        assert!(!is_office_binary(PDF));
        assert!(!is_office_binary(JPG));
        assert!(!is_office_binary(HTML_AS_XLSX));
        assert!(!is_office_binary(&[0xDE, 0xAD]));
    }

    #[test]
    fn soffice_ext_maps_ole_unknown_to_doc() {
        // An OLE2 file that sniffed Unknown is almost always legacy Word.
        assert_eq!(soffice_ext(DocFormat::Unknown), "doc");
        assert_eq!(soffice_ext(DocFormat::Ppt), "ppt");
        assert_eq!(soffice_ext(DocFormat::Xls), "xls");
    }

    /// Live (skips without LibreOffice): the legacy `.ppt` that `legacy_ppt`
    /// returns `None` for must now extract real text via the soffice fallback.
    /// Validated on the seed box (where soffice is installed); skipped in CI.
    #[tokio::test]
    async fn extract_text_legacy_ppt_via_soffice_when_available() {
        let soffice_ok = std::process::Command::new("soffice")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !soffice_ok {
            eprintln!("skipping: soffice (LibreOffice) not installed");
            return;
        }
        let t = extract_text("/x.ppt", PPT).await.expect("legacy .ppt via soffice");
        assert!(
            t.trim().chars().count() > 20,
            "expected real slide text from soffice, got: {:?}",
            t.chars().take(60).collect::<String>()
        );
    }

    #[test]
    fn sniffs_extension_lie_html_as_html_not_xlsx() {
        // Named `.xlsx`, actually a 403 HTML page. `sniff` classifies by content,
        // so it returns Html (NOT Xlsx) — it would never be mis-sent to calamine.
        // (In the flush path this file is valid UTF-8 and indexed as source text;
        // the Html arm matters for non-UTF-8 HTML.)
        assert_eq!(sniff(HTML_AS_XLSX), DocFormat::Html);
    }

    #[test]
    fn sniffs_garbage_as_unknown() {
        assert_eq!(sniff(&[0xDE, 0xAD, 0xBE, 0xEF]), DocFormat::Unknown);
    }

    #[test]
    fn sniffs_empty_as_unknown() {
        assert_eq!(sniff(&[]), DocFormat::Unknown);
    }

    const ASCII_PDF: &[u8] = include_bytes!("../../tests/fixtures/ascii_textlayer.pdf");

    #[tokio::test]
    async fn extract_text_routes_docx() {
        let t = extract_text("/x.docx", DOCX).await.expect("docx text");
        assert!(t.contains("数据安全风险整改进度月度汇总报告"));
    }

    #[tokio::test]
    async fn extract_text_routes_xlsx() {
        let t = extract_text("/x.xlsx", XLSX).await.expect("xlsx text");
        assert!(t.contains("Changan Automobile"));
    }

    /// Live (skips without `OPENROUTER_API_KEY`): the wired xlsx path indexes a
    /// summary ONLY (embed-only) — the indexed text is the LLM coverage summary,
    /// not the raw cells, so retrieval/rerank run on the semantic summary.
    #[tokio::test]
    async fn extract_text_xlsx_summary_only_when_keyed() {
        if std::env::var("OPENROUTER_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .is_none()
        {
            eprintln!("skipping: OPENROUTER_API_KEY not set");
            return;
        }
        let t = extract_text("/desktop/product-sales/x.xlsx", XLSX).await.expect("xlsx text");
        assert!(!t.trim().is_empty(), "summary present");
        // Embed-only: the result is a summary, NOT the raw-cell extraction.
        let raw: String = spreadsheet::extract_sheets(XLSX)
            .iter()
            .map(|s| s.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert_ne!(t, raw, "indexed text should be the summary, not raw-cell fallback");
    }

    #[tokio::test]
    async fn extract_text_routes_pdf_latin() {
        let t = extract_text("/x.pdf", ASCII_PDF).await.expect("pdf text");
        assert!(t.contains("invoice total 4200"));
    }

    #[tokio::test]
    async fn extract_text_legacy_ppt_unindexed_without_soffice() {
        // Without LibreOffice the pure-Rust path has no legacy-`.ppt` decoder, so
        // the file is accounted unindexed (never dropped). WITH soffice it now
        // extracts — see `extract_text_legacy_ppt_via_soffice_when_available`.
        let soffice_ok = std::process::Command::new("soffice")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if soffice_ok {
            eprintln!("skipping: soffice present (covered by the via_soffice test)");
            return;
        }
        assert_eq!(extract_text("/x.ppt", PPT).await, None);
    }

    #[tokio::test]
    async fn extract_text_unknown_is_none() {
        assert_eq!(
            extract_text("/x.bin", &[0xDE, 0xAD, 0xBE, 0xEF]).await,
            None
        );
    }

    /// Build a minimal `.docx` (zip + one `<w:t>` run) holding `text_bytes` of
    /// text. Repeated chars compress, so the returned buffer stays tiny while the
    /// extracted text is large — exercises the size cap without a big fixture.
    fn make_big_docx(text_bytes: usize) -> Vec<u8> {
        use std::io::Write;
        let body = "A".repeat(text_bytes);
        let xml = format!(
            "<?xml version=\"1.0\"?><w:document xmlns:w=\"x\"><w:body><w:p><w:r>\
             <w:t>{body}</w:t></w:r></w:p></w:body></w:document>"
        );
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            zw.start_file(
                "word/document.xml",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zw.write_all(xml.as_bytes()).unwrap();
            zw.finish().unwrap();
        }
        buf
    }

    #[tokio::test]
    async fn extract_text_caps_oversized_output() {
        // 2 MiB of extracted text must be truncated to the retrieval-head cap so
        // one giant document can't drive unbounded chunking/embedding (RCA
        // 2026-06-03-extract-unbounded-large-doc-hang).
        let docx = make_big_docx(2 * 1024 * 1024);
        let out = extract_text("/big.docx", &docx)
            .await
            .expect("docx extracts");
        assert!(
            out.len() <= MAX_EXTRACT_BYTES,
            "output not capped: {} bytes",
            out.len()
        );
    }

    #[test]
    fn cap_text_truncates_on_char_boundary_no_panic() {
        // Multibyte fill (each '中' = 3 bytes) past the cap; truncation must land
        // on a char boundary (String::truncate would panic otherwise).
        let capped = cap_text("中".repeat(MAX_EXTRACT_BYTES));
        assert!(capped.len() <= MAX_EXTRACT_BYTES);
        assert!(capped.chars().all(|c| c == '中'), "split a multibyte char");
    }

    #[test]
    fn cap_text_leaves_small_text_untouched() {
        assert_eq!(cap_text("small".into()), "small");
    }

    #[tokio::test]
    async fn blocking_times_out_a_slow_extractor() {
        use std::time::Duration;
        let r = blocking(
            || {
                std::thread::sleep(Duration::from_millis(400));
                Some("too late".to_string())
            },
            Duration::from_millis(20),
        )
        .await;
        assert_eq!(
            r, None,
            "a slow extractor must time out to None (unindexed)"
        );
    }
}

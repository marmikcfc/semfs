//! Vision extraction via OpenRouter `gpt-4.1-mini` — the one deliberate network
//! exception in local extraction: image OCR ([`ocr_image`]) and the PDF fallback
//! ([`ocr_pdf`]) for files the pure-Rust text layer can't decode. Key-gated on
//! `OPENROUTER_API_KEY` exactly like the L7 graph LLM: no key (or air-gapped run)
//! ⇒ `None`, so the file lands in the unindexed bucket rather than blocking the
//! import. The caller runs these in `spawn_blocking`.

const OCR_PROMPT: &str = "Transcribe all text visible in this image verbatim. \
Output only the transcribed text with no commentary. If the image contains no \
text, output nothing.";

/// OCR an image's bytes to text. `None` when no API key is configured or the
/// request/transcription is empty or fails.
pub fn ocr_image(bytes: &[u8]) -> Option<String> {
    ocr_image_with_key(api_key(), bytes)
}

/// The configured OpenRouter key, or `None` if unset/blank.
fn api_key() -> Option<String> {
    std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
}

/// Largest image we will base64-encode and send. Beyond this the request is
/// pointless (provider limits) and the base64 expansion (~1.33×) wastes memory,
/// so we skip → unindexed rather than balloon a flush.
const MAX_OCR_BYTES: usize = 10 * 1024 * 1024;

/// Gate + request, split out so the key-gate is testable without the network.
fn ocr_image_with_key(key: Option<String>, bytes: &[u8]) -> Option<String> {
    let key = key.filter(|k| !k.trim().is_empty())?;
    if bytes.len() > MAX_OCR_BYTES {
        tracing::warn!(len = bytes.len(), "image exceeds OCR size cap; skipping");
        return None;
    }
    // Only JPEG reaches here (sniff routes `FFD8FF` → Jpeg), so the data-URL
    // media type is correct for every input on this path.
    let data_url = format!("data:image/jpeg;base64,{}", base64_encode(bytes));
    let body = serde_json::json!({
        "model": "openai/gpt-4.1-mini",
        "temperature": 0.0,
        "max_tokens": 2048,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": OCR_PROMPT },
                { "type": "image_url", "image_url": { "url": data_url } },
            ],
        }],
    });
    let resp: serde_json::Value = crate::http::timeout_agent()
        .post("https://openrouter.ai/api/v1/chat/completions")
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .send_json(body)
        .ok()?
        .into_json()
        .ok()?;
    reject_non_transcription(resp["choices"][0]["message"]["content"].as_str()?)
}

/// Reject a model refusal / "no text" reply so a non-answer is never indexed as
/// document content. When OCR fails to read a scan (corrupt or empty image) the
/// model often replies with an apology or a "[no text]" note instead of a
/// transcription — that is not the file's content, and indexing it pollutes
/// retrieval (RCA 2026-06-06-pdf-ocr-fallback…). Matched case-insensitively on
/// the reply's head. Empty ⇒ `None`.
fn reject_non_transcription(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let head = t.chars().take(64).collect::<String>().to_lowercase();
    // Phrases a transcription would never open with; an OCR refusal/no-text reply does.
    const REFUSALS: &[&str] = &[
        "sorry",
        "i'm sorry",
        "i am sorry",
        "i can't",
        "i cannot",
        "i can not",
        "i'm unable",
        "i am unable",
        "i'm not able",
        "unable to transcribe",
        "can't transcribe",
        "cannot transcribe",
        "the document contains no text",
        "[the document contains no text",
        "there is no text",
        "contains no text",
    ];
    if REFUSALS.iter().any(|r| head.contains(r)) {
        tracing::debug!(reply = %head, "OCR reply is a refusal/no-text; treating as unindexed");
        return None;
    }
    Some(t.to_string())
}

const PDF_OCR_PROMPT: &str = "Transcribe ALL text in this document verbatim, in reading \
order. Output only the document's text with no commentary or summary. If it contains no \
text, output nothing.";

/// Extract a PDF's text via `gpt-4.1-mini`, used as the fallback when the
/// pure-Rust text layer can't be decoded (scanned PDFs, or CJK CID fonts
/// `pdf-extract` chokes on). OpenRouter's `file-parser` plugin with the
/// `mistral-ocr` engine rasterizes + OCRs the PDF — the `native` engine only
/// passes a PDF's existing text layer to the model and the upstream provider
/// returns HTTP 400 `unsupported_file` on image-only scans (RCA
/// 2026-06-06-pdf-ocr-fallback-native-engine-rejects-scanned-pdfs), which is
/// exactly the class this fallback exists for. Key-gated and size-capped like
/// image OCR: no key / oversized / empty / failed ⇒ `None` (caller → unindexed).
pub fn ocr_pdf(bytes: &[u8]) -> Option<String> {
    ocr_pdf_with_key(api_key(), bytes)
}

fn ocr_pdf_with_key(key: Option<String>, bytes: &[u8]) -> Option<String> {
    let key = key.filter(|k| !k.trim().is_empty())?;
    if bytes.len() > MAX_OCR_BYTES {
        tracing::warn!(len = bytes.len(), "pdf exceeds OCR size cap; skipping");
        return None;
    }
    let data_url = format!("data:application/pdf;base64,{}", base64_encode(bytes));
    let body = serde_json::json!({
        "model": "openai/gpt-4.1-mini",
        "temperature": 0.0,
        "max_tokens": 8192,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": PDF_OCR_PROMPT },
                { "type": "file", "file": { "filename": "document.pdf", "file_data": data_url } },
            ],
        }],
        "plugins": [{ "id": "file-parser", "pdf": { "engine": "mistral-ocr" } }],
    });
    let resp: serde_json::Value = pdf_ocr_agent()
        .post("https://openrouter.ai/api/v1/chat/completions")
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .send_json(body)
        .ok()?
        .into_json()
        .ok()?;
    reject_non_transcription(resp["choices"][0]["message"]["content"].as_str()?)
}

/// Cap on pages OCR'd per scanned PDF. A scan's leading pages (cover, TOC,
/// summary, intro) carry the searchable signal; bounding the count keeps a
/// 141-page scan from fanning out 141 vision calls. Pages beyond this are not
/// indexed (a documented partial — better than the whole file unindexed).
const MAX_OCR_PAGES: usize = 40;
/// In-flight per-page vision calls. Bounded so the fan-out can't open hundreds
/// of sockets at once on a big scan.
const OCR_PAGE_CONCURRENCY: usize = 6;

/// OCR a scanned PDF **page by page** via poppler `pdftoppm` + per-page vision.
/// This is the path for image-only scans (no text layer) — especially LARGE ones
/// (multi-MB / many pages) where sending the whole PDF as one request blows the
/// provider's size limit and the request timeout. Each page rasterizes to a small
/// downscaled JPEG (~100 KB) that OCRs in ~5 s, so any-size scan stays within
/// per-request bounds. Returns `None` if poppler is absent, no key is set, or no
/// page yields text (caller → unindexed). Pages are OCR'd with bounded
/// parallelism and re-joined in reading order.
pub fn ocr_pdf_paged(bytes: &[u8]) -> Option<String> {
    api_key()?; // no key ⇒ unindexed, no work
    let dir = tempfile::tempdir().ok()?;
    let input = dir.path().join("in.pdf");
    std::fs::write(&input, bytes).ok()?;
    // Rasterize the first MAX_OCR_PAGES pages to downscaled JPEGs (longest side
    // 1600px) — small enough that each page is well under the OCR size cap.
    let status = std::process::Command::new("pdftoppm")
        .args(["-jpeg", "-scale-to", "1600", "-f", "1", "-l"])
        .arg(MAX_OCR_PAGES.to_string())
        .arg(&input)
        .arg(dir.path().join("pg"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        return None; // poppler absent, or a PDF it can't rasterize (malformed)
    }
    let mut pages: Vec<std::path::PathBuf> = std::fs::read_dir(dir.path())
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("jpg"))
        .collect();
    pages.sort(); // pg-01.jpg, pg-02.jpg, … → reading order
    if pages.is_empty() {
        return None;
    }
    // Fan out per-page OCR with bounded concurrency, preserving page order.
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out: Vec<std::sync::Mutex<Option<String>>> =
        pages.iter().map(|_| std::sync::Mutex::new(None)).collect();
    let workers = OCR_PAGE_CONCURRENCY.min(pages.len());
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if i >= pages.len() {
                    break;
                }
                if let Ok(img) = std::fs::read(&pages[i]) {
                    if let Some(t) = ocr_image(&img) {
                        *out[i].lock().expect("ocr page mutex") = Some(t);
                    }
                }
            });
        }
    });
    let joined = out
        .into_iter()
        .filter_map(|m| m.into_inner().ok().flatten())
        .collect::<Vec<_>>()
        .join("\n");
    if joined.trim().is_empty() {
        None
    } else {
        Some(joined)
    }
}

// ── Merged vision tier (transcribe-or-describe) ─────────────────────────────
// One vision call that TRANSCRIBES visible text if present, else DESCRIBES the
// image — eliminating the fragile "did OCR find text?" routing. On a textless
// image, plain OCR returns junk ("5") or a stray description that slips past the
// refusal filter and pre-empts a real description; the merged prompt instead has
// the model self-signal "no text" via a sentinel, so a description is recognized
// and MARKED (never mistaken for the file's own text). Opt-in (the caller gates
// on `SEMFS_VLM_DESCRIBE`). See tickets/vlm-describe-images.

const VISION_PROMPT: &str = "Transcribe ALL visible text in this image verbatim, in \
reading order, with no commentary. If the image has no meaningful text (a photo, chart, \
diagram, or blank/near-blank page), do NOT apologize — instead write a concise factual \
visual description and begin your reply with the exact token @@VISUAL@@ on its own. \
Output ONLY the verbatim text, or the @@VISUAL@@ description.";

/// Sentinel the model prefixes when it describes rather than transcribes (no real text).
const VISUAL_SENTINEL: &str = "@@VISUAL@@";

/// Prefixed to indexed text that is a VISION DESCRIPTION (not the file's own
/// text) so retrieval — and any downstream agent/judge — never cites a described
/// image as authoritative source text.
pub const DESCRIPTION_MARKER: &str =
    "[IMAGE DESCRIPTION — generated by a vision model from a visual-only document; not the file's own text]";

/// Representative pages to vision-process for a scanned/visual PDF or office doc.
const MAX_VISION_PAGES: usize = 10;

/// One merged vision call → `(text, is_description)`. `is_description` is true
/// when the model signaled no real text via [`VISUAL_SENTINEL`]. `None` on
/// no-key/oversize/empty/hard-refusal.
fn vision_one(key: &str, bytes: &[u8]) -> Option<(String, bool)> {
    if bytes.len() > MAX_OCR_BYTES {
        tracing::warn!(len = bytes.len(), "image exceeds vision size cap; skipping");
        return None;
    }
    let data_url = format!("data:image/jpeg;base64,{}", base64_encode(bytes));
    let body = serde_json::json!({
        "model": "openai/gpt-4.1-mini",
        "temperature": 0.0,
        "max_tokens": 2048,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": VISION_PROMPT },
                { "type": "image_url", "image_url": { "url": data_url } },
            ],
        }],
    });
    let resp: serde_json::Value = crate::http::timeout_agent()
        .post("https://openrouter.ai/api/v1/chat/completions")
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .send_json(body)
        .ok()?
        .into_json()
        .ok()?;
    classify_vision(resp["choices"][0]["message"]["content"].as_str()?)
}

/// Parse a merged-vision reply into `(text, is_description)`. Empty / hard-refusal
/// ⇒ `None`. A reply led by [`VISUAL_SENTINEL`] is a description (sentinel stripped).
fn classify_vision(reply: &str) -> Option<(String, bool)> {
    let t = reply.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(rest) = t.strip_prefix(VISUAL_SENTINEL) {
        let d = rest.trim();
        return if d.is_empty() {
            None
        } else {
            Some((d.to_string(), true))
        };
    }
    // A real transcription — but still drop a hard refusal the model emitted anyway.
    let head = t.chars().take(32).collect::<String>().to_lowercase();
    const REFUSALS: &[&str] = &[
        "sorry", "i can't", "i cannot", "i can not", "i'm unable", "i am unable", "i'm not able",
    ];
    if REFUSALS.iter().any(|r| head.contains(r)) {
        return None;
    }
    Some((t.to_string(), false))
}

/// Merged vision extraction for a JPEG: transcribed text as-is, or a
/// [`DESCRIPTION_MARKER`]-prefixed description when the image has no text. `None`
/// without a key.
pub fn vision_extract_image(bytes: &[u8]) -> Option<String> {
    let key = api_key()?;
    vision_one(&key, bytes).map(|(t, is_desc)| {
        if is_desc {
            format!("{DESCRIPTION_MARKER}\n{t}")
        } else {
            t
        }
    })
}

/// Merged vision extraction for a scanned/visual PDF: rasterize the first
/// [`MAX_VISION_PAGES`] pages (`pdftoppm`) and run the merged call per page.
/// Text pages are transcribed; textless pages are described inline. If EVERY page
/// was a description, the whole result is marked. `None` without poppler/key or
/// if nothing is recoverable.
pub fn vision_extract_pdf_paged(bytes: &[u8]) -> Option<String> {
    let key = api_key()?;
    let dir = tempfile::tempdir().ok()?;
    let input = dir.path().join("in.pdf");
    std::fs::write(&input, bytes).ok()?;
    let status = std::process::Command::new("pdftoppm")
        .args(["-jpeg", "-scale-to", "1600", "-f", "1", "-l"])
        .arg(MAX_VISION_PAGES.to_string())
        .arg(&input)
        .arg(dir.path().join("pg"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let mut pages: Vec<std::path::PathBuf> = std::fs::read_dir(dir.path())
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("jpg"))
        .collect();
    pages.sort();
    let mut parts = Vec::new();
    let mut any_text = false;
    for (i, p) in pages.iter().enumerate() {
        if let Ok(img) = std::fs::read(p) {
            if let Some((t, is_desc)) = vision_one(&key, &img) {
                if is_desc {
                    parts.push(format!("[page {} image] {}", i + 1, t));
                } else {
                    any_text = true;
                    parts.push(format!("[page {}] {}", i + 1, t));
                }
            }
        }
    }
    if parts.is_empty() {
        return None;
    }
    let joined = parts.join("\n");
    // No page had real text ⇒ the whole doc is visual ⇒ mark it.
    Some(if any_text {
        joined
    } else {
        format!("{DESCRIPTION_MARKER}\n{joined}")
    })
}

/// Dedicated agent with a longer read timeout than the shared one: native PDF
/// processing is multi-page and routinely exceeds the shared 30s read budget.
fn pdf_ocr_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout_read(std::time::Duration::from_secs(90))
        .timeout_write(std::time::Duration::from_secs(30))
        .build()
}

/// Standard base64 (RFC 4648) — small enough to inline rather than pull a crate
/// (two base64 versions are already in the lockfile transitively).
fn base64_encode(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const JPG: &[u8] = include_bytes!("../../tests/fixtures/chanpin/sample.jpg");
    // Real chanpin PDF: CJK CID font pdf-extract can't decode — the exact case the
    // gpt-4.1-mini fallback exists for.
    const PDF: &[u8] = include_bytes!("../../tests/fixtures/chanpin/sample.pdf");

    #[test]
    fn no_key_yields_none_no_network() {
        // The acceptance criterion: absent key ⇒ unindexed, never a hang/error.
        assert_eq!(ocr_image_with_key(None, JPG), None);
        assert_eq!(ocr_image_with_key(Some("   ".into()), JPG), None);
        // Same gate for the PDF fallback — no key ⇒ None, no network.
        assert_eq!(ocr_pdf_with_key(None, PDF), None);
        assert_eq!(ocr_pdf_with_key(Some("   ".into()), PDF), None);
    }

    /// Live test (skips without `OPENROUTER_API_KEY`): the CJK PDF that
    /// `pdf-extract` cannot decode is transcribed to non-empty text by the
    /// gpt-4.1-mini native-PDF fallback.
    #[test]
    fn ocr_pdf_transcribes_cjk_pdf_live() {
        if api_key().is_none() {
            eprintln!("skipping: OPENROUTER_API_KEY not set");
            return;
        }
        let t = ocr_pdf(PDF).expect("PDF OCR fallback should return text with a key set");
        assert!(!t.trim().is_empty(), "PDF OCR returned empty text");
    }

    /// Live (skips without key+poppler): the page-split OCR path rasterizes the
    /// CJK PDF and transcribes its text. This is the scalable path for large
    /// image-only scans where the whole-PDF request is too big to send.
    #[test]
    fn ocr_pdf_paged_transcribes_when_available() {
        if api_key().is_none() {
            eprintln!("skipping: OPENROUTER_API_KEY not set");
            return;
        }
        let have_poppler = std::process::Command::new("pdftoppm")
            .arg("-v")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !have_poppler {
            eprintln!("skipping: pdftoppm (poppler) not installed");
            return;
        }
        let t = ocr_pdf_paged(PDF).expect("paged OCR should transcribe the CJK PDF");
        assert!(!t.trim().is_empty(), "paged OCR returned empty text");
    }

    #[test]
    fn reject_non_transcription_filters_refusals_keeps_content() {
        // Real transcriptions pass through (incl. CJK).
        assert_eq!(
            reject_non_transcription("普华永道2024年数据资产专题报告"),
            Some("普华永道2024年数据资产专题报告".to_string())
        );
        assert_eq!(
            reject_non_transcription("  invoice total 4200  "),
            Some("invoice total 4200".to_string())
        );
        // Refusals / no-text replies are dropped (the corrupt-scan case).
        for r in [
            "Sorry, I can't transcribe this document.",
            "I'm sorry, I cannot transcribe the text from this document.",
            "I cannot directly access or read the contents of the file.",
            "[The document contains no text.]",
            "",
            "   ",
        ] {
            assert_eq!(reject_non_transcription(r), None, "should reject: {r:?}");
        }
    }

    #[test]
    fn classify_vision_splits_transcription_from_description() {
        // A @@VISUAL@@-led reply is a description (sentinel stripped, flagged true).
        assert_eq!(
            classify_vision("@@VISUAL@@ a green cap and a white t-shirt"),
            Some(("a green cap and a white t-shirt".to_string(), true))
        );
        // Plain text is a transcription (flagged false) — kept verbatim, incl. CJK.
        assert_eq!(
            classify_vision("普华永道2024年数据资产专题报告"),
            Some(("普华永道2024年数据资产专题报告".to_string(), false))
        );
        // Empty / sentinel-only / hard refusal ⇒ None.
        for r in ["", "   ", "@@VISUAL@@   ", "Sorry, I can't help with that."] {
            assert_eq!(classify_vision(r), None, "should reject: {r:?}");
        }
    }

    #[test]
    fn vision_extract_image_no_key_yields_none() {
        // Goes through api_key(); absent key ⇒ None with no network call.
        if api_key().is_some() {
            eprintln!("skipping: OPENROUTER_API_KEY is set in this env");
            return;
        }
        assert_eq!(vision_extract_image(JPG), None);
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    /// Live test (skips without `OPENROUTER_API_KEY`): the real product photo
    /// transcribes to some non-empty text.
    #[test]
    fn ocr_transcribes_real_image_live() {
        if api_key().is_none() {
            eprintln!("skipping: OPENROUTER_API_KEY not set");
            return;
        }
        let t = ocr_image(JPG).expect("OCR should return text with a key set");
        assert!(!t.trim().is_empty(), "OCR returned empty text");
    }
}

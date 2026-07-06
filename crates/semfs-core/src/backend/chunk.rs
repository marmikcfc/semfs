//! L1 chunking — overlap-aware word windows over the verbatim source.
//!
//! Replaces the old "split at ~1000 chars" approach. Chunks are **verbatim
//! substrings** of the original content (so grep can map a chunk back to file
//! line ranges) aligned to word boundaries, with overlap between consecutive
//! chunks so a fact straddling a boundary still lands wholly inside one chunk.

/// Chunking knobs. Words are a coarse proxy for tokens; defaults keep a chunk
/// comfortably within small local models' sequence limits while staying useful.
#[derive(Debug, Clone, Copy)]
pub struct ChunkOptions {
    pub max_words: usize,
    pub overlap_words: usize,
}

impl Default for ChunkOptions {
    fn default() -> Self {
        Self {
            max_words: 200,
            overlap_words: 30,
        }
    }
}

/// Hard per-file ceiling on the content handed to the indexer. Chunk count
/// (→ embedding time + memory) MUST be bounded per file: a large file that is
/// valid UTF-8 — e.g. a minified `node_modules` bundle, or a huge extracted
/// spreadsheet — otherwise chunks into thousands of windows and stalls the whole
/// import on the embed lane (RCAs `2026-06-03-extract-unbounded-large-doc-hang`
/// and `…-uncapped-utf8-text-path-node-modules-hang`). Applied at `index()` so it
/// bounds EVERY source — plain text, code, and extracted document text alike. A
/// retrieval index only needs the document's head.
pub const MAX_INDEX_BYTES: usize = 1024 * 1024; // 1 MiB

/// Borrow at most `MAX_INDEX_BYTES` of `content`, truncated to a UTF-8 char
/// boundary so the slice stays valid (cheap — no allocation).
pub fn cap_index_content(content: &str) -> &str {
    if content.len() <= MAX_INDEX_BYTES {
        return content;
    }
    let mut end = MAX_INDEX_BYTES;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    &content[..end]
}

/// Split `content` into overlapping, word-aligned, verbatim chunks.
pub fn recursive_chunks(content: &str, opts: &ChunkOptions) -> Vec<String> {
    // Byte spans of each whitespace-delimited word, over the ORIGINAL content.
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut start: Option<usize> = None;
    for (idx, ch) in content.char_indices() {
        if ch.is_whitespace() {
            if let Some(s) = start.take() {
                spans.push((s, idx));
            }
        } else if start.is_none() {
            start = Some(idx);
        }
    }
    if let Some(s) = start {
        spans.push((s, content.len()));
    }

    if spans.is_empty() {
        return vec![];
    }
    let max = opts.max_words.max(1);
    if spans.len() <= max {
        // Whole thing is one chunk (trimmed to the first/last word, verbatim between).
        return vec![content[spans[0].0..spans[spans.len() - 1].1].to_string()];
    }

    // Overlapping windows: each chunk is `max` words, stepping by max-overlap.
    let step = max.saturating_sub(opts.overlap_words).max(1);
    let mut chunks = Vec::new();
    let mut i = 0;
    loop {
        let end = (i + max).min(spans.len());
        chunks.push(content[spans[i].0..spans[end - 1].1].to_string());
        if end == spans.len() {
            break;
        }
        i += step;
    }
    chunks
}

// ── v2 chunking (opt-in via SEMFS_CHUNK_V2): AST-aware code + context headers ──

use super::graph_ast::DefSpan;

/// A chunk of a file: the **verbatim** body plus an optional retrieval-only
/// context header. The header is prepended to the EMBED and BM25 input via
/// [`Chunk::indexed`], but callers store `text` (not `indexed`) in the `chunks`
/// table so grep still maps a hit back to exact file line ranges.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub text: String,
    pub header: Option<String>,
    /// 1-based inclusive line range of `text` in the ORIGINAL file, so grep can
    /// deliver `path:line_start-line_end` and the agent can `cat` the rest.
    /// `0` = unknown (legacy v1 word-window path, which has no clean line mapping).
    pub line_start: usize,
    pub line_end: usize,
}

/// 1-based inclusive line range of the byte span `[start, end)` within `src`.
fn line_span(src: &str, start: usize, end: usize) -> (usize, usize) {
    let b = src.as_bytes();
    let nl = |upto: usize| b[..upto.min(b.len())].iter().filter(|&&c| c == b'\n').count();
    let ls = 1 + nl(start);
    let last = end.saturating_sub(1).max(start);
    (ls, (1 + nl(last)).max(ls))
}

impl Chunk {
    /// Header + verbatim body — the string to embed and index for BM25. With no
    /// header (the legacy path) this is exactly the verbatim text (no regression).
    pub fn indexed(&self) -> String {
        match &self.header {
            Some(h) if !h.is_empty() => format!("{h}\n{}", self.text),
            _ => self.text.clone(),
        }
    }
}

/// Char budget for the v2 chunker. ~2000 chars ≈ 500 tokens — cAST's empirical
/// sweet spot for code retrieval, and a far better unit than 200 whitespace-words.
pub const CHUNK_CHAR_BUDGET: usize = 2000;

/// Split `s` into ≤`budget`-char windows at LINE boundaries (keeps code/prose
/// lines intact). Each window is a verbatim contiguous substring of `s`.
/// Byte spans `[start, end)` of each ≤`budget` line-boundary window over `s`.
/// Callers slice `s[start..end]` for the verbatim window and compute its line
/// range against the original source via [`line_span`].
fn line_windows_spans(s: &str, budget: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0usize; // start byte of the current window
    let mut pos = 0usize; // running end byte of the current window
    for line in s.split_inclusive('\n') {
        let llen = line.len();
        if pos > start && (pos - start) + llen > budget {
            out.push((start, pos));
            start = pos;
        }
        pos += llen;
        if pos - start >= budget {
            out.push((start, pos)); // a single over-budget line stands alone
            start = pos;
        }
    }
    if pos > start && !s[start..pos].trim().is_empty() {
        out.push((start, pos));
    }
    out
}

fn base_header(path: &str, imports: &[String]) -> String {
    let mut h = format!("# {path}");
    if !imports.is_empty() {
        let imps: Vec<&str> = imports.iter().take(8).map(|s| s.as_str()).collect();
        h.push_str(&format!(" · imports: {}", imps.join(", ")));
    }
    h
}

/// First non-blank line of a slice — the def's signature line, for the header.
fn signature_line(s: &str) -> &str {
    s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim()
}

/// AST-aware code chunking (split-then-merge): cut on top-level def boundaries,
/// keep whole defs together, greedily merge small consecutive units up to
/// `budget`, and char-split any single def that exceeds it. Each chunk carries a
/// context header (path + imports; plus the def signature for single-def chunks).
/// `None` if the file isn't a supported language / doesn't parse / has no defs —
/// the caller then falls back to the prose chunker.
pub fn code_chunks(path: &str, src: &str, budget: usize) -> Option<Vec<Chunk>> {
    let (defs, imports) = super::graph_ast::def_spans(path, src)?;
    if defs.is_empty() {
        return None;
    }
    let base = base_header(path, &imports);

    // Contiguous segments over [0, src.len()]: inter-def gaps + each def span.
    let mut segs: Vec<(usize, usize)> = Vec::new();
    let mut cut = 0;
    for d in &defs {
        if d.start > cut {
            segs.push((cut, d.start));
        }
        segs.push((d.start, d.end));
        cut = d.end.max(cut);
    }
    if cut < src.len() {
        segs.push((cut, src.len()));
    }

    // Header from the def(s) fully contained in a span (exactly one → its label).
    let header_for = |gs: usize, ge: usize| -> Option<String> {
        let inside: Vec<&DefSpan> = defs.iter().filter(|d| d.start >= gs && d.end <= ge).collect();
        if inside.len() == 1 {
            let d = inside[0];
            Some(format!(
                "{base} · {} {}\n#   {}",
                d.kind.as_str(),
                d.name,
                signature_line(&src[d.start..d.end])
            ))
        } else {
            Some(base.clone())
        }
    };

    let mut chunks: Vec<Chunk> = Vec::new();
    let mut i = 0;
    while i < segs.len() {
        let (gs, seg_end) = segs[i];
        if seg_end - gs > budget {
            // oversized single segment (a huge function, or a big module-level gap)
            let hdr = header_for(gs, seg_end);
            for (ws, we) in line_windows_spans(&src[gs..seg_end], budget) {
                let (as_, ae) = (gs + ws, gs + we);
                let text = &src[as_..ae];
                if !text.trim().is_empty() {
                    let (ls, le) = line_span(src, as_, ae);
                    chunks.push(Chunk {
                        text: text.to_string(),
                        header: hdr.clone(),
                        line_start: ls,
                        line_end: le,
                    });
                }
            }
            i += 1;
            continue;
        }
        // greedily extend the group while it still fits the budget
        let mut ge = seg_end;
        let mut j = i + 1;
        while j < segs.len() && segs[j].1 - gs <= budget {
            ge = segs[j].1;
            j += 1;
        }
        let text = &src[gs..ge];
        if !text.trim().is_empty() {
            let (ls, le) = line_span(src, gs, ge);
            chunks.push(Chunk {
                text: text.to_string(),
                header: header_for(gs, ge),
                line_start: ls,
                line_end: le,
            });
        }
        i = j;
    }
    Some(chunks)
}

/// Top-level v2 chunker: AST-aware for code, char-budget line windows (+ a path
/// header) for prose/text. Every chunk's `text` is a verbatim substring of
/// `content` (so grep line-mapping survives).
pub fn chunk_file(path: &str, content: &str, budget: usize) -> Vec<Chunk> {
    if let Some(cs) = code_chunks(path, content, budget) {
        return cs;
    }
    let base = format!("# {path}");
    line_windows_spans(content, budget)
        .into_iter()
        .filter(|&(a, b)| !content[a..b].trim().is_empty())
        .map(|(a, b)| {
            let (ls, le) = line_span(content, a, b);
            Chunk {
                text: content[a..b].to_string(),
                header: Some(base.clone()),
                line_start: ls,
                line_end: le,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<&str> {
        s.split_whitespace().collect()
    }

    const PY: &str = "def alpha():\n    return 1\n\ndef beta():\n    return 2\n\ndef gamma():\n    return 3\n";

    #[test]
    fn code_chunks_split_on_function_boundaries() {
        // Small budget so each function is its own chunk (no merge).
        let chunks = code_chunks("svc/pay.py", PY, 30).expect("python parses");
        assert_eq!(chunks.len(), 3, "one chunk per function, got {chunks:?}");
        for (c, body) in chunks.iter().zip(["return 1", "return 2", "return 3"]) {
            assert!(PY.contains(c.text.as_str()), "chunk not verbatim: {:?}", c.text);
            assert!(c.text.contains(body), "function body split: {:?}", c.text);
        }
    }

    #[test]
    fn code_chunks_report_line_ranges() {
        // budget 30 → each function is its own chunk; ranges must bracket the body's real line.
        let chunks = code_chunks("svc/pay.py", PY, 30).expect("python parses");
        for c in &chunks {
            let off = PY.find(c.text.as_str()).expect("verbatim");
            let body_line = PY[..off].matches('\n').count() + 1; // 1-based line of chunk start
            assert!(c.line_start >= 1, "line numbers are 1-based: {c:?}");
            assert!(
                c.line_start <= body_line && body_line <= c.line_end,
                "chunk {:?} reports lines {}-{} but its text starts at line {}",
                c.text, c.line_start, c.line_end, body_line
            );
        }
        assert_eq!(chunks[0].line_start, 1, "first function starts at line 1");
    }

    #[test]
    fn prose_chunks_report_line_ranges() {
        let doc = "line one\nline two\nline three\n";
        let chunks = chunk_file("notes/memo.txt", doc, CHUNK_CHAR_BUDGET);
        assert_eq!(chunks[0].line_start, 1);
        assert!(chunks[0].line_end >= 1);
    }

    #[test]
    fn code_chunk_header_has_path_and_scope() {
        let chunks = code_chunks("svc/pay.py", PY, 30).unwrap();
        let h0 = chunks[0].header.as_deref().unwrap();
        assert!(h0.contains("svc/pay.py"), "header lacks path: {h0}");
        assert!(h0.contains("alpha"), "header lacks def name: {h0}");
        assert!(chunks[0].indexed().starts_with("# svc/pay.py"));
        assert!(!chunks[0].text.starts_with('#'), "stored text must be verbatim");
    }

    #[test]
    fn code_chunks_merge_small_defs_under_budget() {
        // Big budget → all three tiny functions merge into one chunk.
        let chunks = code_chunks("svc/pay.py", PY, CHUNK_CHAR_BUDGET).unwrap();
        assert_eq!(chunks.len(), 1, "tiny defs should merge under a 2k budget");
        assert!(PY.contains(chunks[0].text.as_str()), "merged chunk stays verbatim");
    }

    #[test]
    fn chunk_file_prose_uses_path_header_and_stays_verbatim() {
        let doc = "First paragraph line.\nSecond line here.\n";
        let chunks = chunk_file("notes/memo.txt", doc, CHUNK_CHAR_BUDGET);
        assert!(!chunks.is_empty());
        for c in &chunks {
            assert!(doc.contains(c.text.as_str()), "prose chunk not verbatim");
            assert_eq!(c.header.as_deref(), Some("# notes/memo.txt"));
        }
    }

    #[test]
    fn indexed_no_header_equals_text() {
        let c = Chunk { text: "raw body".into(), header: None, line_start: 0, line_end: 0 };
        assert_eq!(c.indexed(), "raw body"); // legacy path: no regression
    }

    #[test]
    fn empty_and_short() {
        assert!(recursive_chunks("   ", &ChunkOptions::default()).is_empty());
        let one = recursive_chunks("short text here", &ChunkOptions::default());
        assert_eq!(one, vec!["short text here".to_string()]);
    }

    #[test]
    fn long_doc_splits_into_overlapping_verbatim_chunks() {
        let opts = ChunkOptions {
            max_words: 10,
            overlap_words: 3,
        };
        // 25 numbered words.
        let content = (1..=25)
            .map(|n| format!("w{n}"))
            .collect::<Vec<_>>()
            .join(" ");
        let chunks = recursive_chunks(&content, &opts);
        assert!(
            chunks.len() >= 3,
            "expected multiple chunks, got {}",
            chunks.len()
        );

        // Every chunk: <= max_words words, and a verbatim substring of content.
        for c in &chunks {
            assert!(words(c).len() <= opts.max_words);
            assert!(content.contains(c.as_str()), "chunk not verbatim: {c:?}");
        }

        // Consecutive chunks overlap: the tail of chunk k reappears at the head
        // of chunk k+1 (so a boundary-straddling fact is wholly inside one chunk).
        for k in 0..chunks.len() - 1 {
            let tail = words(&chunks[k]);
            let head = words(&chunks[k + 1]);
            let tail_last = &tail[tail.len() - opts.overlap_words..];
            assert_eq!(
                tail_last,
                &head[..opts.overlap_words],
                "chunks {k}/{} do not overlap",
                k + 1
            );
        }
    }

    #[test]
    fn covers_all_words_in_order() {
        let opts = ChunkOptions {
            max_words: 5,
            overlap_words: 2,
        };
        let content = (1..=12)
            .map(|n| format!("t{n}"))
            .collect::<Vec<_>>()
            .join(" ");
        let chunks = recursive_chunks(&content, &opts);
        // First word in first chunk, last word in last chunk.
        assert!(chunks[0].starts_with("t1"));
        assert!(chunks.last().unwrap().ends_with("t12"));
    }

    #[test]
    fn cap_index_content_leaves_small_untouched() {
        assert_eq!(cap_index_content("small content"), "small content");
    }

    #[test]
    fn cap_index_content_truncates_on_char_boundary() {
        // Multibyte fill (each '中' = 3 bytes) past the cap; the slice must land on
        // a char boundary (slicing mid-char would panic).
        let big = "中".repeat(MAX_INDEX_BYTES); // 3 × MAX_INDEX_BYTES bytes
        let capped = cap_index_content(&big);
        assert!(capped.len() <= MAX_INDEX_BYTES);
        assert!(capped.chars().all(|c| c == '中'), "split a multibyte char");
    }
}

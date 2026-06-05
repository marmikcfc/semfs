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

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<&str> {
        s.split_whitespace().collect()
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
        assert!(chunks.len() >= 3, "expected multiple chunks, got {}", chunks.len());

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
        let content = (1..=12).map(|n| format!("t{n}")).collect::<Vec<_>>().join(" ");
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

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
}

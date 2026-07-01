//! Offline extraction pass for files Supermemory rejected server-side (the
//! `.semfs-error.txt` set). Reuses the PRODUCTION `extract::extract_text`
//! (magic-byte sniff + OCR fallback) so the recovered text is identical to what
//! the local L1 indexer would store — no second, divergent extractor. Writes a
//! `<path>.extracted.md` text sibling next to each source that yields text.
//!
//! Feeds BOTH backends fairly: the `.md` sidecars are plain text, so the local
//! seed indexes them and a cloud mount pushes them as text docs Supermemory
//! accepts (it rejected the mislabeled binaries).
//!
//! Run: OPENROUTER_API_KEY=... cargo run --release -p semfs-core \
//!        --example extract_failed -- <listfile-of-abs-paths>

use std::io::Write;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> anyhow::Result<()> {
    let listfile = std::env::args()
        .nth(1)
        .expect("usage: extract_failed <listfile-of-abs-paths>");
    let paths: Vec<String> = std::fs::read_to_string(&listfile)?
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let ocr_on = std::env::var("OPENROUTER_API_KEY")
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false);
    eprintln!(
        "processing {} files; OCR {}",
        paths.len(),
        if ocr_on {
            "ENABLED"
        } else {
            "DISABLED (no key)"
        }
    );

    let (mut ok, mut miss, mut read_err) = (0usize, 0usize, 0usize);
    let mut by_fmt: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    for p in &paths {
        let bytes = match std::fs::read(p) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("READ-ERR {p}: {e}");
                read_err += 1;
                continue;
            }
        };
        let fmt = format!("{:?}", semfs_core::extract::sniff(&bytes));
        let name = p.rsplit('/').next().unwrap_or(p);
        let entry = by_fmt.entry(fmt.clone()).or_default();
        match semfs_core::extract::extract_text(p, &bytes).await {
            Some(t) if !t.trim().is_empty() => {
                let out = format!("{p}.extracted.md");
                std::fs::File::create(&out)?.write_all(t.as_bytes())?;
                println!("OK   {:<8} {:>8}B  {name}", fmt, t.len());
                ok += 1;
                entry.0 += 1;
            }
            _ => {
                println!("MISS {:<8}           {name}", fmt);
                miss += 1;
                entry.1 += 1;
            }
        }
    }
    eprintln!("\n=== by true format (recovered/miss) ===");
    for (f, (o, m)) in &by_fmt {
        eprintln!("  {f:<8} recovered={o} miss={m}");
    }
    eprintln!(
        "\n=== recovered={ok} miss={miss} read_err={read_err} total={} ===",
        paths.len()
    );
    Ok(())
}

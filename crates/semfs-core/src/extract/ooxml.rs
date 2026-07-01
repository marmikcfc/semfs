//! OOXML (docx/pptx) text extraction — unzip the document part and join the
//! text runs. docx text lives in `<w:t>` runs inside `word/document.xml`;
//! pptx text lives in `<a:t>` runs across `ppt/slides/slideN.xml`.

use quick_xml::events::Event;
use quick_xml::name::QName;
use quick_xml::reader::Reader;
use std::io::{Cursor, Read};

// Borrow the caller's buffer (Cursor<&[u8]> is Read + Seek) — no extra copy of
// the whole file just to open the zip.
type Zip<'a> = zip::ZipArchive<Cursor<&'a [u8]>>;

/// Extract the visible text of a `.docx` (OOXML Word). `None` if the zip or its
/// main document part can't be read, or the result is empty.
pub fn extract_docx(bytes: &[u8]) -> Option<String> {
    let mut zip = open_zip(bytes)?;
    let xml = read_entry(&mut zip, "word/document.xml")?;
    non_empty(join_text_runs(&xml))
}

/// Extract the visible text of a `.pptx` (OOXML PowerPoint), concatenating all
/// slides in numeric order. `None` if the zip can't be read or has no slide text.
pub fn extract_pptx(bytes: &[u8]) -> Option<String> {
    let mut zip = open_zip(bytes)?;
    // Collect slide parts, ordered by their numeric suffix (slide1, slide2, …).
    let mut slides: Vec<(u32, String)> = zip
        .file_names()
        .filter_map(|n| slide_number(n).map(|num| (num, n.to_string())))
        .collect();
    slides.sort_by_key(|(num, _)| *num);
    let mut out = String::new();
    for (_, name) in slides {
        if let Some(xml) = read_entry(&mut zip, &name) {
            out.push_str(&join_text_runs(&xml));
            out.push('\n');
        }
    }
    non_empty(out)
}

fn open_zip(bytes: &[u8]) -> Option<Zip<'_>> {
    zip::ZipArchive::new(Cursor::new(bytes)).ok()
}

fn read_entry(zip: &mut Zip<'_>, name: &str) -> Option<String> {
    let mut f = zip.by_name(name).ok()?;
    let mut buf = String::new();
    f.read_to_string(&mut buf).ok()?;
    Some(buf)
}

/// `ppt/slides/slideN.xml` → `N`; anything else → `None`.
fn slide_number(name: &str) -> Option<u32> {
    let stem = name
        .strip_prefix("ppt/slides/slide")?
        .strip_suffix(".xml")?;
    stem.parse().ok()
}

/// Walk OOXML markup, concatenating text inside `<*:t>` runs and emitting a
/// newline at each paragraph (`<*:p>`) close so search keeps word boundaries.
fn join_text_runs(xml: &str) -> String {
    let mut reader = Reader::from_str(xml);
    let mut out = String::new();
    let mut in_text_run = false;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if local_is(e.name(), b"t") => in_text_run = true,
            Ok(Event::End(e)) => match e.name().local_name().as_ref() {
                b"t" => in_text_run = false,
                b"p" => out.push('\n'),
                _ => {}
            },
            Ok(Event::Text(t)) if in_text_run => {
                out.push_str(&t.unescape().unwrap_or_default());
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

fn local_is(name: QName, want: &[u8]) -> bool {
    name.local_name().as_ref() == want
}

fn non_empty(s: String) -> Option<String> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOCX: &[u8] = include_bytes!("../../tests/fixtures/chanpin/sample.docx");
    const PPTX: &[u8] = include_bytes!("../../tests/fixtures/chanpin/sample.pptx");

    #[test]
    fn extracts_docx_cjk_text() {
        let t = extract_docx(DOCX).expect("docx should extract");
        assert!(
            t.contains("数据安全风险整改进度月度汇总报告"),
            "missing the report title; got: {}",
            &t.chars().take(80).collect::<String>()
        );
    }

    #[test]
    fn extracts_pptx_text() {
        let t = extract_pptx(PPTX).expect("pptx should extract");
        assert!(
            t.contains("NovaLCT"),
            "missing NovaLCT; got: {}",
            &t.chars().take(80).collect::<String>()
        );
        assert!(t.contains("V5.8.1"), "missing version string");
    }

    #[test]
    fn garbage_docx_returns_none_no_panic() {
        assert_eq!(extract_docx(&[0x50, 0x4B, 0x03, 0x04, 0xDE, 0xAD]), None);
    }
}

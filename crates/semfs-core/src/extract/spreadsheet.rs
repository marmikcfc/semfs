//! Spreadsheet text extraction via `calamine`, which reads both OOXML `.xlsx`
//! and legacy OLE2 `.xls`. Each sheet is the natural *table* unit, so we extract
//! per-sheet ([`extract_sheets`]) rather than flattening the whole workbook into
//! one blob — the summary-augmented retrieval path summarizes each sheet on its
//! own (see `tickets/summary-augmented-table-retrieval/`). Layout/formatting is
//! intentionally dropped; only non-empty cell text is kept.

use calamine::{open_workbook_auto_from_rs, Reader};
use std::io::Cursor;

/// One sheet's worth of extracted cell text — the table unit we summarize and
/// embed separately. `text` is tab/newline-joined non-empty cells (no layout).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sheet {
    pub name: String,
    pub text: String,
}

/// Extract every sheet of a workbook (xlsx or xls) as a separate [`Sheet`].
/// Sheets with no non-empty cells are skipped. Returns an empty vec if the
/// workbook can't be opened or has no cell text anywhere.
pub fn extract_sheets(bytes: &[u8]) -> Vec<Sheet> {
    // Borrow the buffer (Cursor<&[u8]> is Read + Seek) — no whole-file copy.
    let Ok(mut wb) = open_workbook_auto_from_rs(Cursor::new(bytes)) else {
        return vec![];
    };
    let mut sheets = Vec::new();
    for name in wb.sheet_names() {
        let Ok(range) = wb.worksheet_range(&name) else {
            continue;
        };
        let mut text = String::new();
        for row in range.rows() {
            let cells: Vec<String> = row
                .iter()
                .map(|c| c.to_string())
                .filter(|s| !s.trim().is_empty())
                .collect();
            if !cells.is_empty() {
                text.push_str(&cells.join("\t"));
                text.push('\n');
            }
        }
        if !text.trim().is_empty() {
            sheets.push(Sheet { name, text });
        }
    }
    sheets
}

#[cfg(test)]
mod tests {
    use super::*;

    const XLSX: &[u8] = include_bytes!("../../tests/fixtures/chanpin/sample.xlsx");
    const XLS: &[u8] = include_bytes!("../../tests/fixtures/chanpin/sample.xls");

    #[test]
    fn extract_sheets_yields_named_per_sheet_units() {
        // The table unit is the sheet: we must get at least one named sheet whose
        // own text carries the cell content (not one flattened workbook blob).
        let sheets = extract_sheets(XLSX);
        assert!(!sheets.is_empty(), "expected at least one sheet");
        assert!(
            sheets.iter().all(|s| !s.name.trim().is_empty()),
            "every sheet must carry its name"
        );
        assert!(
            sheets.iter().any(|s| s.text.contains("Changan Automobile")),
            "a sheet's own text should hold the known cell"
        );
    }

    #[test]
    fn extract_sheets_empty_on_garbage() {
        assert!(extract_sheets(&[0xD0, 0xCF, 0x11, 0xE0, 0x00]).is_empty());
    }

    #[test]
    fn extract_sheets_reads_legacy_xls() {
        // calamine reads both OOXML and legacy OLE2; a known header must land in
        // some sheet's text (per-sheet, not flattened).
        let sheets = extract_sheets(XLS);
        assert!(
            sheets
                .iter()
                .any(|s| s.text.contains("Product Quality Issue Analysis")),
            "missing known xls header across sheets"
        );
    }
}

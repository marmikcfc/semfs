# chanpin extractor fixtures (real corpus samples)

Real files pulled from the `chanpin_raw` workspace on the benchmark host
(`/srv/semfs-benchmark/Workspace-Bench/evaluation/filesys/chanpin_raw`, EC2
`i-0c491c7cc23de8555`). Used to TDD the local document extractors (see
`tickets/local-document-extractors/issue.md`). Content is **real and CJK** —
synthetic fixtures would not exercise the multibyte path or the corpus's
extension-lies messiness.

> Privacy/size note: these are benchmark-corpus documents (Workspace-Bench),
> not private user data. Total ~250 KB. Keep the set minimal.

## Happy-path fixtures — one real file per format

| File | True type (`file -b`) | Bytes | Verified assertion string (substring that must appear in extracted text) |
|---|---|---:|---|
| `sample.docx` | Microsoft Word 2007+ (OOXML) | 18 703 | `数据安全风险整改进度月度汇总报告` (data-security risk remediation monthly summary) |
| `sample.xlsx` | Microsoft Excel 2007+ (OOXML) | 5 154 | `Chongqing Changan Automobile` / `Production volume` |
| `sample.pptx` | Microsoft OOXML | 47 430 | `NovaLCT V5.8.1` / `NovaStar` |
| `sample.pdf` | PDF 1.7, 8 pages, CJK **CID font** | 21 162 | **KNOWN GAP** — pdf-extract panics on the non-Identity-H CMap; contained → unindexed. (Used in the CJK-gap test.) |
| `sample.xls` | Composite Document V2 (OLE2 legacy) | 11 776 | `Product Quality Issue Analysis` (via calamine) |
| `sample.ppt` | Composite Document V2 (OLE2 legacy) | 129 024 | **DESCOPED** — litchi 0.0.1 returns only `"*"`; routed to unindexed (see issue.md) |
| `sample.jpg` | JPEG 540×720 RGB (clothing product) | 15 729 | _OCR target — live OpenRouter test asserts non-empty transcription_ |

> `tests/fixtures/ascii_textlayer.pdf` (generated, Latin text layer) is the pdf
> extractor's happy-path fixture — `pdf-extract` decodes it; the CJK `sample.pdf`
> above is the contained-panic gap case.

## Edge fixtures — real-world messiness the ticket exists to fix

| File | What it actually is | Why it matters |
|---|---|---|
| `edge_html_masquerading_as.xlsx` | **HTML** — a saved `403 Forbidden` openresty error page | Proves `sniff` routes by content, not extension: it classifies as `Html`, NOT `Xlsx`, so it's never mis-sent to `calamine`. Note: this file is valid UTF-8, so in the real flush path it's indexed as its source text (searchable, not dropped) via the unchanged text path — `sniff`/the `Html→None` dispatcher arm only apply to NON-UTF-8 content. |

## CORPUS FINDING — extension is NOT a reliable type signal

True-type breakdown of the **201 non-empty `.xlsx`** files (by `file -b`):

| Really is | count | correct extractor |
|---|---:|---|
| Microsoft Excel 2007+ | 148 | calamine |
| **PDF** | 20 | pdf-extract |
| Composite Doc (legacy .xls) | 18 | calamine |
| Microsoft OOXML | 9 | calamine |
| **HTML** | 3 | html fallback / unindexed |
| Zip archive | 2 | sniff |
| **Word .docx** | 1 | docx extractor |

`.docx` (52): 50 real Word, **2 are OLE2** (legacy `.doc` renamed). `.xls` (8): all 8 real OLE2.

**Implication for the design:** `extract_text` must route by **content sniffing
(magic bytes)**, not by extension — routing purely on `.xlsx`→calamine would
silently fail ~20% of the real `.xlsx` files (the PDFs/HTML/docx-in-disguise),
re-introducing the exact silent-drop the ticket kills.

## Regenerate / re-pull

```bash
S="ssh -i ~/.ssh/semfs-benchmark ubuntu@13.201.35.159"
B=/srv/semfs-benchmark/Workspace-Bench/evaluation/filesys/chanpin_raw
# see git history of this dir for the exact source paths per fixture
```

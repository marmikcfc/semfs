"""Phase 0 preflight — corpus composition + language for the 3 cells.

Opens each persona seed on the semfs-bench-data volume and reports:
  - code vs doc vs other file counts (→ does the cell have a code lane?)
  - CJK ratio overall + on code files (→ is the code Chinese? gates LateOn-Code fit)

Answers: xAFS English? · kaifa code language? · houqin code presence?
Run:  modal run tickets/next-plaid-late-interaction/phase0_preflight.py
"""
import modal

app = modal.App("phase0-seed-preflight")
vol = modal.Volume.from_name("semfs-bench-data")
image = modal.Image.debian_slim(python_version="3.11")

CODE_EXT = {".go", ".py", ".ts", ".tsx", ".js", ".jsx", ".java", ".rs", ".c", ".h",
            ".cpp", ".cc", ".cs", ".rb", ".php", ".kt", ".swift", ".scala", ".lua",
            ".ex", ".exs", ".vue", ".svelte", ".sh", ".sql", ".r", ".jl", ".zig", ".m"}
DOC_EXT = {".pptx", ".docx", ".doc", ".xlsx", ".xls", ".pdf", ".md", ".txt", ".csv",
           ".json", ".yaml", ".yml", ".toml", ".html", ".htm", ".rtf"}


@app.function(image=image, volumes={"/data": vol}, timeout=900, cpu=2.0)
def preflight():
    import sqlite3, os, collections

    def cjk_ratio(s):
        if not s:
            return 0.0
        cjk = sum(1 for ch in s if "一" <= ch <= "鿿")
        alpha = sum(1 for ch in s if ch.isalpha() or "一" <= ch <= "鿿")
        return cjk / alpha if alpha else 0.0

    out = {}
    for persona in ["kaifa", "houqin", "xafs"]:
        p = f"/data/seeds/{persona}-gemma-q4.db"
        if not os.path.exists(p):
            out[persona] = {"error": "seed missing"}
            continue
        con = sqlite3.connect(f"file:{p}?mode=ro", uri=True)
        cur = con.cursor()
        try:
            files = [r[0] for r in cur.execute("SELECT DISTINCT filepath FROM chunks").fetchall() if r[0]]
        except Exception as e:
            out[persona] = {"error": f"chunks read: {e!r}"}
            con.close()
            continue
        code = docs = other = 0
        for f in files:
            ext = os.path.splitext(f)[1].lower()
            code += ext in CODE_EXT
            docs += ext in DOC_EXT
            other += ext not in CODE_EXT and ext not in DOC_EXT
        samp = cur.execute(
            "SELECT filepath, text FROM chunks WHERE text IS NOT NULL AND length(text)>40 LIMIT 5000"
        ).fetchall()
        overall = [cjk_ratio(t) for _, t in samp][:600]
        code_cjk = [cjk_ratio(t) for fp, t in samp if os.path.splitext(fp)[1].lower() in CODE_EXT][:300]
        out[persona] = {
            "files_total": len(files),
            "code_files": code, "doc_files": docs, "other_files": other,
            "overall_cjk_mean": round(sum(overall) / len(overall), 3) if overall else None,
            "code_cjk_mean": round(sum(code_cjk) / len(code_cjk), 3) if code_cjk else None,
            "code_chunks_sampled": len(code_cjk),
            "top_code_exts": dict(collections.Counter(
                os.path.splitext(f)[1].lower() for f in files
                if os.path.splitext(f)[1].lower() in CODE_EXT).most_common(8)),
        }
        con.close()
    return out


@app.local_entrypoint()
def main():
    import json
    print("PREFLIGHT:\n" + json.dumps(preflight.remote(), indent=2, ensure_ascii=False))

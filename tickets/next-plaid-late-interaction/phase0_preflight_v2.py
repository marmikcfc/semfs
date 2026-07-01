"""Phase 0 preflight v2 — CORRECTED classification.

v1 bug: classified by the trailing extension, so code files suffixed `.md`
(e.g. `analyze_synergy_budgets.py.md`) were counted as docs → "xAFS 0 code"
was a classifier artifact. v2 strips wrapper suffixes (.md/.txt/.extracted) to
find the INNER extension, enumerates real code files, and samples content to
confirm it's code (not prose). Re-runs all 3 personas.

Run: modal run tickets/next-plaid-late-interaction/phase0_preflight_v2.py
"""
import modal

app = modal.App("phase0-preflight-v2")
vol = modal.Volume.from_name("semfs-bench-data")
image = modal.Image.debian_slim(python_version="3.11")

CODE_INNER = {".go", ".py", ".ts", ".tsx", ".js", ".jsx", ".java", ".rs", ".c", ".h",
              ".cpp", ".cc", ".cs", ".rb", ".php", ".kt", ".swift", ".scala", ".lua",
              ".ex", ".exs", ".vue", ".svelte", ".sh", ".bash", ".sql", ".r", ".jl",
              ".zig", ".m", ".pl", ".ps1", ".ipynb"}
WRAP = {".md", ".txt", ".extracted"}


def effective_ext(path):
    import os
    p = path.lower()
    while True:
        root, ext = os.path.splitext(p)
        if ext in WRAP and root:
            p = root
        else:
            break
    return os.path.splitext(p)[1]


def looks_like_code(t):
    if not t:
        return False
    sig = ["def ", "import ", "function ", "class ", "#!/", "return ", "const ",
           "var ", "SELECT ", "library(", "<-", "println", "public ", "void ", "= function"]
    return sum(s in t for s in sig) >= 2


@app.function(image=image, volumes={"/data": vol}, timeout=900, cpu=2.0)
def inspect():
    import sqlite3, os, collections

    def cjk(s):
        if not s:
            return 0.0
        c = sum(1 for ch in s if "一" <= ch <= "鿿")
        a = sum(1 for ch in s if ch.isalpha() or "一" <= ch <= "鿿")
        return c / a if a else 0.0

    out = {}
    for persona in ["xafs", "kaifa", "houqin"]:
        p = f"/data/seeds/{persona}-gemma-q4.db"
        con = sqlite3.connect(f"file:{p}?mode=ro", uri=True)
        cur = con.cursor()
        files = [r[0] for r in cur.execute("SELECT DISTINCT filepath FROM chunks").fetchall() if r[0]]
        inner = collections.Counter(effective_ext(f) for f in files)
        code_files = [f for f in files if effective_ext(f) in CODE_INNER]
        # sample content of code files
        samples = []
        for f in code_files[:6]:
            row = cur.execute("SELECT text FROM chunks WHERE filepath=? AND text IS NOT NULL LIMIT 1", (f,)).fetchone()
            t = (row[0] if row else "") or ""
            samples.append({"path": f, "cjk": round(cjk(t), 3), "looks_like_code": looks_like_code(t), "head": t[:180].replace("\n", "⏎")})
        out[persona] = {
            "files_total": len(files),
            "true_code_files": len(code_files),
            "top_inner_exts": inner.most_common(25),
            "code_paths_sample": code_files[:25],
            "content_samples": samples,
        }
        con.close()
    return out


@app.local_entrypoint()
def main():
    import json
    print("PREFLIGHT_V2:\n" + json.dumps(inspect.remote(), indent=2, ensure_ascii=False))

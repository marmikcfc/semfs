"""Confirm the graphify-parity KG is built in the kaifa (BackendDeveloper) seed,
and measure the KG's token footprint.

Run:
  modal run benchmarks/modal/kg_token_audit.py
  modal run benchmarks/modal/kg_token_audit.py --db-name kaifa-gemma-q4.db

What it checks (graphify parity = ALL of these populated, not just rows>0):
  1. graph_entity   : count + file_type breakdown (code|document|…) + rationale coverage
  2. graph_relation : count + typed-relation breakdown + confidence breakdown
  3. graph_community / graph_god_node : Louvain projection present

Token footprint (measured on the STORED KG content, the unambiguous payload size):
  - entities  : name + kind + rationale + source_file, per row
  - relations : source/target/relation triple, per row (stored slug form)
  - relations (name-resolved) : triple with /memories/<slug>.md resolved to entity names
  Tokenized with o200k_base (codex/GPT-4o/5 family — the WB agent) and cl100k_base (cross-check).
"""
import modal

app = modal.App("kg-token-audit")
vol = modal.Volume.from_name("semfs-bench-data")
VOL = "/data"

img = (
    modal.Image.debian_slim(python_version="3.11")
    .apt_install("sqlite3")
    .pip_install("tiktoken>=0.7")
)


@app.function(image=img, volumes={VOL: vol}, timeout=900)
def kg_audit(db_name: str = "kaifa-gemma-q4.db") -> dict:
    import os, sqlite3, json

    seeds_dir = f"{VOL}/seeds"
    db = f"{seeds_dir}/{db_name}"
    available = sorted(os.listdir(seeds_dir)) if os.path.isdir(seeds_dir) else []
    out = {
        "db": db,
        "db_exists": os.path.exists(db),
        "db_size_mb": round(os.path.getsize(db) / 1e6, 1) if os.path.exists(db) else None,
        "seeds_available": available,
    }
    if not os.path.exists(db):
        out["error"] = "seed db not found on volume"
        return out

    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)

    def has_table(t):
        return con.execute(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", (t,)
        ).fetchone() is not None

    def q1(sql, params=()):
        try:
            r = con.execute(sql, params).fetchone()
            return r[0] if r else None
        except Exception as e:
            return f"err:{e}"

    def qall(sql, params=()):
        try:
            return con.execute(sql, params).fetchall()
        except Exception as e:
            return [("err", str(e))]

    # ---- 1) graphify-parity structure ------------------------------------
    parity = {
        "graph_entity_exists": has_table("graph_entity"),
        "graph_relation_exists": has_table("graph_relation"),
        "graph_community_exists": has_table("graph_community"),
        "graph_god_node_exists": has_table("graph_god_node"),
    }
    if has_table("graph_entity"):
        parity["entities_total"] = q1("SELECT COUNT(*) FROM graph_entity")
        parity["entities_with_file_type"] = q1(
            "SELECT COUNT(*) FROM graph_entity WHERE file_type IS NOT NULL AND file_type<>''"
        )
        parity["entities_with_rationale"] = q1(
            "SELECT COUNT(*) FROM graph_entity WHERE rationale IS NOT NULL AND rationale<>''"
        )
        parity["file_type_breakdown"] = {
            k: v for k, v in qall(
                "SELECT COALESCE(file_type,'(null)'), COUNT(*) FROM graph_entity "
                "GROUP BY file_type ORDER BY 2 DESC"
            )
        }
        parity["kind_breakdown"] = {
            str(k): v for k, v in qall(
                "SELECT kind, COUNT(*) FROM graph_entity GROUP BY kind ORDER BY 2 DESC LIMIT 15"
            )
        }
    if has_table("graph_relation"):
        parity["relations_total"] = q1("SELECT COUNT(*) FROM graph_relation")
        parity["relation_type_breakdown"] = {
            str(k): v for k, v in qall(
                "SELECT relation, COUNT(*) FROM graph_relation GROUP BY relation ORDER BY 2 DESC"
            )
        }
        parity["confidence_breakdown"] = {
            str(k): v for k, v in qall(
                "SELECT confidence, COUNT(*) FROM graph_relation GROUP BY confidence ORDER BY 2 DESC"
            )
        }
    if has_table("graph_community"):
        parity["communities"] = q1("SELECT COUNT(DISTINCT community_id) FROM graph_community")
        parity["files_in_communities"] = q1("SELECT COUNT(*) FROM graph_community")
    if has_table("graph_god_node"):
        parity["god_nodes"] = q1("SELECT COUNT(*) FROM graph_god_node")

    # parity verdict: every graphify signal populated
    parity["VERDICT_graphify_parity_built"] = bool(
        parity.get("entities_total") and parity.get("relations_total")
        and (parity.get("entities_with_file_type") or 0) > 0
        and len(parity.get("relation_type_breakdown", {})) >= 1
        and (parity.get("communities") or 0) > 0
        and (parity.get("god_nodes") or 0) > 0
    )

    # ---- 2) token footprint of the stored KG -----------------------------
    # entity text: name | kind | rationale | source_file
    ent_rows = qall(
        "SELECT path, COALESCE(name,''), COALESCE(kind,''), COALESCE(rationale,''), "
        "COALESCE(source_file,'') FROM graph_entity"
    ) if has_table("graph_entity") else []
    name_by_path = {r[0]: r[1] for r in ent_rows}
    entities_text = "\n".join(
        f"{name} | {kind} | {rationale} | {src}"
        for (_p, name, kind, rationale, src) in ent_rows
    )
    rationale_text = "\n".join(r[3] for r in ent_rows if r[3])

    rel_rows = qall(
        "SELECT source, target, relation FROM graph_relation"
    ) if has_table("graph_relation") else []
    relations_text_stored = "\n".join(f"{s} {rel} {t}" for (s, t, rel) in rel_rows)
    # name-resolved (closer to what the agent reads in GRAPH_REPORT / node files)
    def nm(p):
        return name_by_path.get(p, p.rsplit("/", 1)[-1].replace(".md", ""))
    relations_text_named = "\n".join(f"{nm(s)} —[{rel}]→ {nm(t)}" for (s, t, rel) in rel_rows)

    full_kg_text = entities_text + "\n" + relations_text_named

    def tok(enc_name, text):
        import tiktoken
        try:
            enc = tiktoken.get_encoding(enc_name)
            return len(enc.encode(text))
        except Exception as e:
            return f"err:{e}"

    segments = {
        "entities (name+kind+rationale+src)": entities_text,
        "rationale only": rationale_text,
        "relations (stored slug triple)": relations_text_stored,
        "relations (name-resolved)": relations_text_named,
        "FULL KG (entities + named relations)": full_kg_text,
    }
    tokens = {}
    for label, text in segments.items():
        tokens[label] = {
            "chars": len(text),
            "tokens_o200k_base": tok("o200k_base", text),   # codex / GPT-4o,5 family
            "tokens_cl100k_base": tok("cl100k_base", text),  # GPT-4 / cross-check
        }

    out["graphify_parity"] = parity
    out["kg_token_footprint"] = tokens
    return out


@app.local_entrypoint()
def main(db_name: str = "kaifa-gemma-q4.db"):
    import json
    print(json.dumps(kg_audit.remote(db_name), ensure_ascii=False, indent=2))

#!/usr/bin/env python3
"""Generate a cached in-context WORKSPACE MAP from a semfs seed.

Two layers, both distilled to fit a token budget (default ~4.8k):
  1. FS skeleton      — directory tree (depth 2) + file counts + top extensions + landmark files
  2. Community overlay — Leiden communities labelled by dominant dir + their top DISTINCTIVE
                         entities (low file-degree = precise pins; dates/bare numbers filtered out)

The map is meant to be prepended to the agent's prompt (cached) so it navigates to the right
directory itself instead of relying on ranked retrieval. See tickets/wblite-ppr-ab/.

Usage: python3 semfs_map.py <seed.db> [--budget 4800] [--out map.txt]
"""
import sqlite3, collections, os, re, argparse, sys

_DATE = re.compile(r"^\d{4}[-/.]\d{1,2}([-/.]\d{1,2})?$")
_NUMy = re.compile(r"^[\d.,%¥$€\s-]+$")

def _good_entity(name):
    n = (name or "").strip()
    if len(n) < 2 or len(n) > 30:
        return False
    if _DATE.match(n) or _NUMy.match(n):
        return False
    if not re.search(r"[A-Za-z一-鿿]", n):   # must contain a letter (latin or CJK)
        return False
    return True

def _toptbl(conn):
    return {r[0] for r in conn.execute("SELECT name FROM sqlite_master WHERE type='table'")}

def build_map(db, budget=4800, fs_landmarks=2, ents_per_comm=4, max_deg=4, prefix="", max_dirs=40):
    conn = sqlite3.connect(db)
    tbls = _toptbl(conn)
    # prefix scopes the map to ONE workspace subtree (e.g. /dp_001/) so a multi-workspace
    # seed (xAFS: 13 people in one db) yields a per-question map, not a noisy whole-corpus one.
    like = (prefix.rstrip("/") + "/%") if prefix else None
    if like:
        paths = [r[0] for r in conn.execute(
            "SELECT DISTINCT filepath FROM chunks WHERE filepath LIKE ?", (like,))]
    else:
        paths = [r[0] for r in conn.execute(
            "SELECT DISTINCT filepath FROM chunks WHERE filepath IS NOT NULL")]
    nfiles = len(paths)

    # ---- entity name + file-degree (distinctiveness) ----
    ename, deg, ent_files = {}, collections.Counter(), collections.defaultdict(set)
    if "graph_entity" in tbls:
        ename = {r[0]: r[1] for r in conn.execute("SELECT path,name FROM graph_entity")}
    if "edges" in tbls:
        _ecur = (conn.execute("SELECT from_path,to_path FROM edges WHERE from_path LIKE ?", (like,))
                 if like else conn.execute("SELECT from_path,to_path FROM edges"))
        for f, e in _ecur:
            deg[e] += 1; ent_files[e].add(f)

    # ---- community overlay (the distilled KG layer) ----
    overlay = []
    if "graph_community" in tbls:
        comm = collections.defaultdict(list)
        _ccur = (conn.execute("SELECT community_id,file_path FROM graph_community WHERE file_path LIKE ?", (like,))
                 if like else conn.execute("SELECT community_id,file_path FROM graph_community"))
        for cid, fp in _ccur:
            comm[cid].append(fp)
        for cid, fps in sorted(comm.items(), key=lambda x: -len(x[1])):
            fset = set(fps)
            cand = collections.Counter()
            for e in {e for f in fset for e in ()}:  # noop placeholder
                pass
            # score entities by in-community frequency, keep only distinctive + named
            for e, fs in ent_files.items():
                inc = len(fs & fset)
                if not inc or deg[e] > max_deg:
                    continue
                nm = ename.get(e, e.split("/")[-1].replace(".md", ""))
                if not _good_entity(nm):
                    continue
                cand[nm] = max(cand[nm], inc * 100 - deg[e])
            top = [nm for nm, _ in cand.most_common(ents_per_comm)]
            dirs = collections.Counter("/".join(p.strip("/").split("/")[:2]) for p in fps)
            dom = "/" + dirs.most_common(1)[0][0] + "/"
            line = f"C{cid} ({len(fps)}f) {dom}"
            if top:
                line += " · " + ", ".join(top)
            overlay.append(line)

    # ---- FS skeleton ----
    tree = collections.defaultdict(lambda: {"n": 0, "ext": collections.Counter(), "ex": []})
    for p in paths:
        parts = p.strip("/").split("/")
        d = "/".join(parts[:2]) if len(parts) > 1 else parts[0]
        t = tree[d]; t["n"] += 1; t["ext"][os.path.splitext(p)[1].lower() or "noext"] += 1
        if len(t["ex"]) < fs_landmarks:
            t["ex"].append(parts[-1][:32])
    fs = []
    for d, t in sorted(tree.items(), key=lambda x: -x[1]["n"]):
        exts = " ".join(f"{e}×{n}" for e, n in t["ext"].most_common(3))
        fs.append(f"/{d}/ ({t['n']}: {exts}) e.g. {', '.join(t['ex'])}")

    # ---- assemble under budget (chars/4 ≈ tokens); trim overlay tail, then dir tail ----
    head = [f"# WORKSPACE MAP — {nfiles} files, {len(tree)} dirs, {len(overlay)} topic-clusters.",
            "# Use it to pick the directory/cluster to read, then grep/cat there.", ""]
    def render(fs_, ov):
        return "\n".join(head + ["## DIRECTORIES"] + fs_ + ["", "## TOPIC CLUSTERS (label · key entities)"] + ov)
    # big workspaces (e.g. dp_012: 3306 dirs) blow the budget on the DIR list alone, which would
    # squeeze out the KG topic-clusters (ppr_map's distinctive value). Cap dirs FIRST (top by
    # file-count, fs is sorted desc) so clusters keep budget room, then trim overlay/dir tails.
    ov, fs_ = overlay, fs[:max_dirs]
    while ov and len(render(fs_, ov)) // 4 > budget:
        ov = ov[:-1]
    while len(fs_) > 1 and len(render(fs_, ov)) // 4 > budget:
        fs_ = fs_[:-1]
    m = render(fs_, ov)
    return m, {"files": nfiles, "dirs": len(tree), "dirs_kept": len(fs_), "clusters_total": len(overlay),
               "clusters_kept": len(ov), "tokens": len(m) // 4}

if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("db")
    ap.add_argument("--budget", type=int, default=4800)
    ap.add_argument("--out", default=None)
    ap.add_argument("--prefix", default="", help="scope map to a subtree, e.g. /dp_001")
    a = ap.parse_args()
    m, stats = build_map(a.db, budget=a.budget, prefix=a.prefix)
    if a.out:
        open(a.out, "w").write(m)
        print(f"wrote {a.out}: {stats}", file=sys.stderr)
    else:
        print(m)
        print(f"\n--- {stats} ---", file=sys.stderr)

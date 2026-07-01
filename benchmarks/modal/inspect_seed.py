"""Inspect a seed DB in-place on the Modal volume (no download).

Answers two questions for `chanpin-gemma-q4.db`:
  1. imported vs indexed vs extraction-failed (the seed-completeness gap).
  2. Are the UNINDEXED files empty (st_size==0) or do they carry real bytes?
     -> settles "didn't we skip reseeding because the remaining files were empty?"

Run:  modal run benchmarks/modal/inspect_seed.py --db chanpin-gemma-q4.db
"""
import modal

app = modal.App("semfs-seed-inspect")
vol = modal.Volume.from_name("semfs-bench-data")
VOL = "/data"
image = modal.Image.debian_slim().pip_install("rich")


@app.function(image=image, volumes={VOL: vol}, timeout=600)
def inspect(db: str = "chanpin-gemma-q4.db"):
    import sqlite3, os, json

    path = f"{VOL}/seeds/{db}"
    print(f"== seed: {path}  ({os.path.getsize(path)/1e6:.1f} MB) ==\n")
    con = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    cur = con.cursor()

    # what tables exist?
    tabs = [r[0] for r in cur.execute(
        "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")]
    print("tables:", ", ".join(tabs), "\n")

    def cols(t):
        try:
            return [r[1] for r in cur.execute(f"PRAGMA table_info({t})")]
        except Exception as e:
            return [f"<err {e}>"]

    # ---- the completeness gap (S_IFREG = 0o100000; mask 0o170000) ----
    REG = "(mode & 32768) = 32768 AND (mode & 61440) = 32768"
    imported = cur.execute(
        f"SELECT COUNT(*) FROM fs_inode WHERE {REG}").fetchone()[0]
    indexed = cur.execute(
        "SELECT COUNT(DISTINCT ino) FROM chunks").fetchone()[0]
    failed = cur.execute(
        "SELECT COUNT(DISTINCT ino) FROM fs_unindexed").fetchone()[0]
    gap = imported - indexed - failed
    cov = 100.0 * indexed / imported if imported else 0.0
    print(f"imported (regular files) : {imported}")
    print(f"indexed   (chunks.ino)   : {indexed}  ({cov:.1f}% coverage)")
    print(f"failed    (fs_unindexed) : {failed}")
    print(f"GAP (imported-idx-fail)  : {gap}\n")

    # ---- are the GAP files empty?  (regular, not indexed, not failed) ----
    cur.execute("""
        CREATE TEMP TABLE gap_ino AS
        SELECT ino, size FROM fs_inode
        WHERE (mode & 32768) = 32768 AND (mode & 61440) = 32768
          AND ino NOT IN (SELECT DISTINCT ino FROM chunks)
          AND ino NOT IN (SELECT DISTINCT ino FROM fs_unindexed)
    """)
    g_total = cur.execute("SELECT COUNT(*) FROM gap_ino").fetchone()[0]
    g_empty = cur.execute("SELECT COUNT(*) FROM gap_ino WHERE size = 0").fetchone()[0]
    g_nonempty = g_total - g_empty
    g_bytes = cur.execute("SELECT COALESCE(SUM(size),0) FROM gap_ino").fetchone()[0]
    print(f"GAP files total          : {g_total}")
    print(f"  empty (size==0)        : {g_empty}")
    print(f"  NON-EMPTY (size>0)     : {g_nonempty}   total {g_bytes/1e6:.1f} MB of unindexed content")

    # size distribution of the non-empty gap files
    if g_nonempty:
        rows = cur.execute(
            "SELECT MIN(size), MAX(size), AVG(size) FROM gap_ino WHERE size>0").fetchone()
        print(f"  non-empty size min/max/avg: {rows[0]} / {rows[1]} / {rows[2]:.0f} bytes")
        # sample some real unindexed files with their paths + extensions
        sample = cur.execute("""
            SELECT d.name, g.size FROM gap_ino g
            JOIN fs_dentry d ON d.ino = g.ino
            WHERE g.size > 0 ORDER BY g.size DESC LIMIT 15
        """).fetchall()
        print("\n  largest non-empty UNINDEXED files (name, bytes):")
        for name, sz in sample:
            print(f"    {sz:>12,}  {name}")
        # extension histogram of non-empty gap files
        ext = cur.execute("""
            SELECT LOWER(SUBSTR(d.name, INSTR(d.name,'.')+1)) ext, COUNT(*) c
            FROM gap_ino g JOIN fs_dentry d ON d.ino=g.ino
            WHERE g.size>0 AND INSTR(d.name,'.')>0
            GROUP BY ext ORDER BY c DESC LIMIT 15
        """).fetchall()
        print("\n  non-empty UNINDEXED by extension:")
        for e, c in ext:
            print(f"    {c:>5}  .{e}")

    # ---- honest accounting: original corpus files vs semfs sidecars ----
    # semfs writes <file>.extracted.md (extraction output) and <file>.semfs-error.txt
    # (extraction failure) as SIBLINGS. Those inflate fs_inode; they are not corpus.
    def cls_counts(where_name):
        return cur.execute(f"""
            SELECT
              SUM(CASE WHEN {where_name} THEN 1 ELSE 0 END),
              SUM(CASE WHEN ({where_name}) AND i.ino IN (SELECT DISTINCT ino FROM chunks) THEN 1 ELSE 0 END)
            FROM fs_inode i JOIN fs_dentry d ON d.ino=i.ino
            WHERE (i.mode & 32768)=32768 AND (i.mode & 61440)=32768 AND i.size>0
        """).fetchone()
    err = "d.name LIKE '%.semfs-error.txt'"
    extr = "d.name LIKE '%.extracted.md'"
    orig = f"NOT ({err}) AND NOT ({extr})"
    print("\n  class                | non-empty files | of which indexed")
    for label, w in (("original corpus", orig), (".extracted.md", extr), (".semfs-error.txt", err)):
        t, ix = cls_counts(w)
        print(f"    {label:18s} | {t or 0:>14} | {ix or 0:>14}")

    con.close()
    return {"imported": imported, "indexed": indexed, "failed": failed, "gap": gap,
            "gap_empty": g_empty, "gap_nonempty": g_nonempty, "gap_bytes": g_bytes}


@app.function(image=image, volumes={VOL: vol}, timeout=600)
def inspect_corpus(corpus: str = "chanpin_standard"):
    """Walk the ORIGINAL corpus on the volume: count files, detect sidecar pollution,
    and total bytes (to decide a download)."""
    import os, collections

    root = f"{VOL}/corpus/{corpus}"
    n_files = n_dirs = total_bytes = 0
    ext = collections.Counter()
    sidecar_extracted = sidecar_error = 0
    empties = 0
    for dirpath, dirs, files in os.walk(root):
        n_dirs += len(dirs)
        for f in files:
            p = os.path.join(dirpath, f)
            try:
                sz = os.path.getsize(p)
            except OSError:
                continue
            n_files += 1
            total_bytes += sz
            if sz == 0:
                empties += 1
            if f.endswith(".extracted.md"):
                sidecar_extracted += 1
            elif f.endswith(".semfs-error.txt"):
                sidecar_error += 1
            else:
                e = f.rsplit(".", 1)[-1].lower() if "." in f else "<noext>"
                ext[e] += 1
    print(f"== corpus: {root} ==")
    print(f"files={n_files}  dirs={n_dirs}  total={total_bytes/1e6:.1f} MB  empty(size==0)={empties}")
    print(f"SIDECAR POLLUTION in corpus: .extracted.md={sidecar_extracted}  .semfs-error.txt={sidecar_error}")
    print(f"original-file count (excl. sidecars) = {n_files - sidecar_extracted - sidecar_error}")
    print("top original extensions:")
    for e, c in ext.most_common(20):
        print(f"  {c:>5}  .{e}")
    return {"files": n_files, "bytes": total_bytes, "empty": empties,
            "sidecar_extracted": sidecar_extracted, "sidecar_error": sidecar_error}


@app.function(image=image, volumes={VOL: vol}, timeout=600)
def true_coverage(db: str = "chanpin-gemma-q4.db"):
    """The decisive metric: of NON-EMPTY ORIGINAL files (excl. sidecars), how many are
    reachable by semfs grep — indexed directly OR via their .extracted.md sibling?
    For the unreachable ones, bucket by why (error sibling / extracted-but-unembedded / none)."""
    import sqlite3
    con = sqlite3.connect(f"file:{VOL}/seeds/{db}?mode=ro", uri=True)
    cur = con.cursor()
    cur.execute("CREATE TEMP TABLE idx AS SELECT DISTINCT ino FROM chunks")
    # original non-empty regular files = not a sidecar, size>0
    cur.execute("""
        CREATE TEMP TABLE orig AS
        SELECT i.ino, d.name, d.parent_ino, i.size
        FROM fs_inode i JOIN fs_dentry d ON d.ino=i.ino
        WHERE (i.mode & 32768)=32768 AND (i.mode & 61440)=32768 AND i.size>0
          AND d.name NOT LIKE '%.extracted.md' AND d.name NOT LIKE '%.semfs-error.txt'
    """)
    total = cur.execute("SELECT COUNT(*) FROM orig").fetchone()[0]
    # direct index
    direct = cur.execute("SELECT COUNT(*) FROM orig WHERE ino IN (SELECT ino FROM idx)").fetchone()[0]
    # reachable via .extracted.md sibling (same parent, name+'.extracted.md', that sibling indexed)
    via_extracted = cur.execute("""
        SELECT COUNT(*) FROM orig o WHERE o.ino NOT IN (SELECT ino FROM idx)
          AND EXISTS (SELECT 1 FROM fs_dentry s JOIN idx ON idx.ino=s.ino
                      WHERE s.parent_ino=o.parent_ino AND s.name = o.name || '.extracted.md')
    """).fetchone()[0]
    reachable = direct + via_extracted
    # unreachable buckets
    unreached = total - reachable
    has_err = cur.execute("""
        SELECT COUNT(*) FROM orig o WHERE o.ino NOT IN (SELECT ino FROM idx)
          AND NOT EXISTS (SELECT 1 FROM fs_dentry s JOIN idx ON idx.ino=s.ino
                          WHERE s.parent_ino=o.parent_ino AND s.name=o.name||'.extracted.md')
          AND EXISTS (SELECT 1 FROM fs_dentry s WHERE s.parent_ino=o.parent_ino AND s.name=o.name||'.semfs-error.txt')
    """).fetchone()[0]
    has_unembedded = cur.execute("""
        SELECT COUNT(*) FROM orig o WHERE o.ino NOT IN (SELECT ino FROM idx)
          AND EXISTS (SELECT 1 FROM fs_dentry s WHERE s.parent_ino=o.parent_ino AND s.name=o.name||'.extracted.md'
                      AND s.ino NOT IN (SELECT ino FROM idx))
          AND NOT EXISTS (SELECT 1 FROM fs_dentry s JOIN idx ON idx.ino=s.ino
                          WHERE s.parent_ino=o.parent_ino AND s.name=o.name||'.extracted.md')
    """).fetchone()[0]
    print(f"NON-EMPTY ORIGINAL files     : {total}")
    print(f"  reachable by grep          : {reachable}  ({100.0*reachable/total:.1f}%)")
    print(f"    - indexed directly       : {direct}")
    print(f"    - via .extracted.md      : {via_extracted}")
    print(f"  UNREACHABLE                : {unreached}")
    print(f"    - extraction FAILED (.semfs-error.txt sibling) : {has_err}")
    print(f"    - extracted but UNEMBEDDED (.extracted.md only): {has_unembedded}")
    print(f"    - other/no sidecar       : {unreached - has_err - has_unembedded}")
    # what are the unreachable real files? sample by extension
    exts = cur.execute("""
        SELECT LOWER(SUBSTR(name, INSTR(name,'.')+1)) ext, COUNT(*) c, SUM(size) b
        FROM orig WHERE ino NOT IN (SELECT ino FROM idx)
          AND NOT EXISTS (SELECT 1 FROM fs_dentry s JOIN idx ON idx.ino=s.ino
                          WHERE s.parent_ino=orig.parent_ino AND s.name=orig.name||'.extracted.md')
        GROUP BY ext ORDER BY c DESC LIMIT 15
    """).fetchall()
    print("\n  UNREACHABLE original files by extension (count, bytes):")
    for e, c, b in exts:
        print(f"    {c:>5}  .{e:8s}  {(b or 0)/1e6:.1f} MB")
    con.close()
    return {"total": total, "reachable": reachable, "unreached": unreached,
            "failed": has_err, "unembedded": has_unembedded}


@app.local_entrypoint()
def main(db: str = "chanpin-gemma-q4.db"):
    inspect.remote(db)


@app.function(image=image, volumes={VOL: vol}, timeout=600)
def confirm_stubs(db: str = "chanpin-gemma-q4.db"):
    """Confirm the 716 .semfs-error.txt stubs come from EMPTY placeholder source files.
    For each error stub, look at its source file (name minus '.semfs-error.txt')."""
    import sqlite3
    con = sqlite3.connect(f"file:{VOL}/seeds/{db}?mode=ro", uri=True)
    cur = con.cursor()
    rows = cur.execute("""
        SELECT
          SUM(CASE WHEN src.size = 0 THEN 1 ELSE 0 END) AS src_empty,
          SUM(CASE WHEN src.size > 0 THEN 1 ELSE 0 END) AS src_nonempty,
          SUM(CASE WHEN src.ino IS NULL THEN 1 ELSE 0 END) AS src_missing,
          COUNT(*) AS total
        FROM fs_dentry e
        LEFT JOIN fs_dentry sd ON sd.parent_ino = e.parent_ino
             AND sd.name = SUBSTR(e.name, 1, LENGTH(e.name) - LENGTH('.semfs-error.txt'))
        LEFT JOIN fs_inode src ON src.ino = sd.ino
        WHERE e.name LIKE '%.semfs-error.txt'
    """).fetchone()
    print(f".semfs-error.txt stubs       : {rows[3]}")
    print(f"  source file EMPTY (size==0): {rows[0]}")
    print(f"  source file NON-EMPTY      : {rows[1]}")
    print(f"  source file MISSING        : {rows[2]}")
    con.close()
    return {"total": rows[3], "src_empty": rows[0], "src_nonempty": rows[1], "src_missing": rows[2]}


@app.local_entrypoint()
def coverage(db: str = "chanpin-gemma-q4.db"):
    true_coverage.remote(db)


@app.function(image=image, volumes={VOL: vol}, timeout=600)
def sample_content(db: str = "chanpin-gemma-q4.db"):
    """Decisive: for binary docs that are BOTH indexed (chunks) AND have a .semfs-error.txt
    sibling, dump the chunk text vs the error text. Tells us if 'indexed' = real content."""
    import sqlite3
    con = sqlite3.connect(f"file:{VOL}/seeds/{db}?mode=ro", uri=True)
    cur = con.cursor()
    # files with chunks AND an error sibling, binary doc types
    rows = cur.execute("""
        SELECT d.ino, d.name, d.parent_ino, i.size
        FROM fs_dentry d JOIN fs_inode i ON i.ino=d.ino
        WHERE i.size>0 AND d.ino IN (SELECT DISTINCT ino FROM chunks)
          AND (d.name LIKE '%.xlsx' OR d.name LIKE '%.pdf' OR d.name LIKE '%.docx')
          AND EXISTS (SELECT 1 FROM fs_dentry s WHERE s.parent_ino=d.parent_ino
                      AND s.name = d.name || '.semfs-error.txt')
        LIMIT 6
    """).fetchall()
    print(f"binary docs that are indexed AND have an error sibling: showing {len(rows)}\n")
    for ino, name, parent, size in rows:
        nch = cur.execute("SELECT COUNT(*) FROM chunks WHERE ino=?", (ino,)).fetchone()[0]
        txt = cur.execute("SELECT text FROM chunks WHERE ino=? ORDER BY ord LIMIT 1", (ino,)).fetchone()
        txt = (txt[0] if txt else "")[:280].replace("\n", " ")
        # error sibling content via fs_data (content store keyed by ino)
        eino = cur.execute("SELECT ino FROM fs_dentry WHERE parent_ino=? AND name=?",
                           (parent, name + ".semfs-error.txt")).fetchone()
        err = ""
        if eino:
            try:
                blob = cur.execute("SELECT data FROM fs_data WHERE ino=? ORDER BY block LIMIT 1",
                                   (eino[0],)).fetchone()
                if blob and blob[0]:
                    err = bytes(blob[0]).decode("utf-8", "replace")[:200].replace("\n", " ")
            except Exception as e:
                err = f"<fs_data read err: {e}>"
        print(f"• {name}  ({size/1e3:.0f} KB, {nch} chunks)")
        print(f"    chunk[0]: {txt!r}")
        print(f"    error:    {err!r}\n")
    # also: how many indexed binary docs have only 1 chunk (a smell for hollow extraction)?
    onech = cur.execute("""
        SELECT COUNT(*) FROM (
          SELECT ino, COUNT(*) c FROM chunks
          WHERE ino IN (SELECT ino FROM fs_dentry WHERE name LIKE '%.xlsx' OR name LIKE '%.pdf' OR name LIKE '%.docx')
          GROUP BY ino HAVING c=1)
    """).fetchone()[0]
    total_bin_idx = cur.execute("""
        SELECT COUNT(DISTINCT ino) FROM chunks
        WHERE ino IN (SELECT ino FROM fs_dentry WHERE name LIKE '%.xlsx' OR name LIKE '%.pdf' OR name LIKE '%.docx')
    """).fetchone()[0]
    print(f"indexed binary docs: {total_bin_idx}; of which single-chunk: {onech}")
    con.close()


@app.local_entrypoint()
def stubs(db: str = "chanpin-gemma-q4.db"):
    confirm_stubs.remote(db)


@app.function(image=image, volumes={VOL: vol}, timeout=600)
def list_unreachable(db: str = "chanpin-gemma-q4.db"):
    """List the genuinely-unreachable non-empty original files (the hard-fails for the report)."""
    import sqlite3
    con = sqlite3.connect(f"file:{VOL}/seeds/{db}?mode=ro", uri=True)
    cur = con.cursor()
    cur.execute("CREATE TEMP TABLE idx AS SELECT DISTINCT ino FROM chunks")
    rows = cur.execute("""
        SELECT d.name, i.size,
          EXISTS(SELECT 1 FROM fs_dentry s WHERE s.parent_ino=d.parent_ino AND s.name=d.name||'.semfs-error.txt') AS has_err,
          EXISTS(SELECT 1 FROM fs_dentry s WHERE s.parent_ino=d.parent_ino AND s.name=d.name||'.extracted.md') AS has_extr
        FROM fs_inode i JOIN fs_dentry d ON d.ino=i.ino
        WHERE (i.mode & 32768)=32768 AND (i.mode & 61440)=32768 AND i.size>0
          AND d.name NOT LIKE '%.extracted.md' AND d.name NOT LIKE '%.semfs-error.txt'
          AND i.ino NOT IN (SELECT ino FROM idx)
          AND NOT EXISTS (SELECT 1 FROM fs_dentry s JOIN idx ON idx.ino=s.ino
                          WHERE s.parent_ino=d.parent_ino AND s.name=d.name||'.extracted.md')
        ORDER BY i.size DESC
    """).fetchall()
    print(f"unreachable non-empty original files: {len(rows)}")
    for name, size, has_err, has_extr in rows:
        why = "extract-FAILED" if has_err else ("extracted-but-unembedded" if has_extr else "no-sidecar")
        print(f"  {size:>10,}  {why:26s}  {name}")
    con.close()


@app.local_entrypoint()
def content(db: str = "chanpin-gemma-q4.db"):
    sample_content.remote(db)


@app.local_entrypoint()
def unreachable(db: str = "chanpin-gemma-q4.db"):
    list_unreachable.remote(db)


@app.local_entrypoint()
def corpus(name: str = "chanpin_standard"):
    inspect_corpus.remote(name)

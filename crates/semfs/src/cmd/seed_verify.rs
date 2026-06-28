//! `semfs seed-verify <db>` — a completeness GATE for a built seed DB.
//!
//! Opens a seed `.db` read-only and answers: of the **real content** in the
//! corpus, how much is reachable by `semfs grep`? "Real content" = non-empty
//! regular files that are NOT semfs's own sidecars (`.extracted.md`,
//! `.semfs-error.txt`). A file is **reachable** if its own inode is chunked OR
//! its `<name>.extracted.md` sibling is chunked.
//!
//! WHY this accounting (not the naive imported−indexed): on the chanpin seed,
//! the naive count reported ~28% because the denominator was padded with 747
//! empty WB placeholder files and 716 stale `.semfs-error.txt` stubs (whose
//! source docs are in fact indexed). The honest metric — non-empty original
//! files reachable by grep — is 616/627 = 98.2%. `fs_unindexed` is unreliable
//! (it held 8 of 716 real extraction fails), so we do NOT use it.
//! See `tickets/seed-completeness-gate/` and the chanpin SEED_COMPLETENESS report.

use anyhow::{Context, Result};
use clap::Args as ClapArgs;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Path to the seed `.db` file to verify (opened read-only).
    pub db: std::path::PathBuf,

    /// Allow up to N unreachable content files and still report COMPLETE
    /// (for known-unextractable formats: legacy .ppt/.xls, scanned PDFs, images).
    #[arg(long, default_value_t = 0)]
    pub allow_unindexed: u64,

    /// Require coverage ≥ P (0.0–1.0). Combined with --allow-unindexed via AND.
    #[arg(long, default_value_t = 0.0)]
    pub min_coverage: f64,

    /// Emit the breakdown as JSON instead of a formatted line.
    #[arg(long)]
    pub json: bool,
}

/// The completeness verdict over a seed's content coverage.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    /// Non-empty original files (excludes semfs sidecars + empty placeholders).
    pub content_files: u64,
    /// Content files reachable by grep (indexed directly or via `.extracted.md`).
    pub reachable: u64,
    /// content_files − reachable.
    pub unreachable: u64,
    /// reachable / content_files (1.0 when there is no content).
    pub coverage: f64,
    /// True iff unreachable ≤ allow_unindexed AND coverage ≥ min_coverage.
    pub complete: bool,
}

/// Pure verdict logic — unit-tested without a database.
pub fn assess(
    content_files: u64,
    reachable: u64,
    allow_unindexed: u64,
    min_coverage: f64,
) -> Verdict {
    let reachable = reachable.min(content_files);
    let unreachable = content_files - reachable;
    let coverage = if content_files == 0 {
        1.0
    } else {
        reachable as f64 / content_files as f64
    };
    let complete = unreachable <= allow_unindexed && coverage >= min_coverage;
    Verdict {
        content_files,
        reachable,
        unreachable,
        coverage,
        complete,
    }
}

pub async fn run(args: Args) -> Result<()> {
    let (content_files, reachable, unreachable_names) =
        count_seed(&args.db).with_context(|| format!("opening seed {:?}", args.db))?;
    let v = assess(
        content_files,
        reachable,
        args.allow_unindexed,
        args.min_coverage,
    );
    if args.json {
        let out = serde_json::json!({
            "db": args.db,
            "content_files": v.content_files,
            "reachable": v.reachable,
            "unreachable": v.unreachable,
            "coverage": v.coverage,
            "complete": v.complete,
            "allow_unindexed": args.allow_unindexed,
            "min_coverage": args.min_coverage,
            "unreachable_files": unreachable_names,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("seed: {:?}", args.db);
        println!(
            "  content files (non-empty, non-sidecar): {}",
            v.content_files
        );
        println!(
            "  reachable by grep                      : {} ({:.1}%)",
            v.reachable,
            v.coverage * 100.0
        );
        println!(
            "  UNREACHABLE                            : {}",
            v.unreachable
        );
        for n in &unreachable_names {
            println!("      - {n}");
        }
        println!(
            "  verdict: {} (allow_unindexed={}, min_coverage={:.2})",
            if v.complete { "COMPLETE" } else { "INCOMPLETE" },
            args.allow_unindexed,
            args.min_coverage
        );
    }
    if !v.complete {
        anyhow::bail!(
            "seed INCOMPLETE: {} unreachable content file(s) exceed allowance {} (coverage {:.1}% < {:.1}%)",
            v.unreachable,
            args.allow_unindexed,
            v.coverage * 100.0,
            args.min_coverage * 100.0
        );
    }
    Ok(())
}

/// Open a seed DB read-only and return (content_files, reachable, unreachable_names).
/// `content_files` = non-empty regular files that are not semfs sidecars.
/// `reachable`      = those whose own ino is chunked, or whose `<name>.extracted.md`
///                    sibling (same parent dir) is chunked.
fn count_seed(db: &std::path::Path) -> Result<(u64, u64, Vec<String>)> {
    use rusqlite::OpenFlags;
    let conn = rusqlite::Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    seed_counts(&conn)
}

/// The SQL accounting, split out so an in-memory DB can exercise it in tests.
fn seed_counts(conn: &rusqlite::Connection) -> Result<(u64, u64, Vec<String>)> {
    conn.execute_batch(
        "CREATE TEMP TABLE _idx AS SELECT DISTINCT ino FROM chunks;
         CREATE TEMP TABLE _content AS
           SELECT i.ino AS ino, d.name AS name, d.parent_ino AS parent_ino
           FROM fs_inode i JOIN fs_dentry d ON d.ino = i.ino
           WHERE (i.mode & 32768) = 32768 AND (i.mode & 61440) = 32768 AND i.size > 0
             AND d.name NOT LIKE '%.extracted.md'
             AND d.name NOT LIKE '%.semfs-error.txt';",
    )?;
    let content_files: u64 = conn.query_row("SELECT COUNT(*) FROM _content", [], |r| r.get(0))?;
    // reachable = own ino indexed, OR a sibling <name>.extracted.md indexed.
    let reachable: u64 = conn.query_row(
        "SELECT COUNT(*) FROM _content c
         WHERE c.ino IN (SELECT ino FROM _idx)
            OR EXISTS (SELECT 1 FROM fs_dentry s JOIN _idx ON _idx.ino = s.ino
                       WHERE s.parent_ino = c.parent_ino
                         AND s.name = c.name || '.extracted.md')",
        [],
        |r| r.get(0),
    )?;
    // names of the unreachable ones (for the report / --json)
    let mut stmt = conn.prepare(
        "SELECT name FROM _content c
         WHERE c.ino NOT IN (SELECT ino FROM _idx)
           AND NOT EXISTS (SELECT 1 FROM fs_dentry s JOIN _idx ON _idx.ino = s.ino
                           WHERE s.parent_ino = c.parent_ino
                             AND s.name = c.name || '.extracted.md')
         ORDER BY name",
    )?;
    let names: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok((content_files, reachable, names))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_when_no_gap() {
        let v = assess(627, 627, 0, 0.0);
        assert_eq!(v.unreachable, 0);
        assert!((v.coverage - 1.0).abs() < 1e-9);
        assert!(v.complete);
    }

    #[test]
    fn incomplete_when_gap_exceeds_allowance() {
        let v = assess(627, 616, 0, 0.0);
        assert_eq!(v.unreachable, 11);
        assert!(
            !v.complete,
            "11 unreachable with allow=0 must be INCOMPLETE"
        );
    }

    #[test]
    fn within_allow_unindexed_is_complete() {
        let v = assess(627, 616, 11, 0.0);
        assert_eq!(v.unreachable, 11);
        assert!(v.complete, "11 unreachable with allow=11 must be COMPLETE");
    }

    #[test]
    fn min_coverage_gate_can_fail_even_within_allowance() {
        // 98.2% coverage, but require 99% → INCOMPLETE despite generous allowance.
        let v = assess(627, 616, 100, 0.99);
        assert!(v.coverage > 0.98 && v.coverage < 0.99);
        assert!(!v.complete);
    }

    #[test]
    fn coverage_math_is_correct() {
        let v = assess(627, 616, 0, 0.0);
        assert!((v.coverage - 616.0 / 627.0).abs() < 1e-9);
    }

    #[test]
    fn empty_seed_is_vacuously_complete() {
        let v = assess(0, 0, 0, 0.0);
        assert!((v.coverage - 1.0).abs() < 1e-9);
        assert!(v.complete);
    }

    #[test]
    fn reachable_cannot_exceed_content_files() {
        // defensive: bogus reachable > content clamps, no underflow panic.
        let v = assess(10, 999, 0, 0.0);
        assert_eq!(v.reachable, 10);
        assert_eq!(v.unreachable, 0);
    }

    // --- integration: exercise the SQL accounting against an in-memory seed ---
    fn build_fixture() -> rusqlite::Connection {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE fs_inode(ino INTEGER PRIMARY KEY, mode INTEGER, size INTEGER);
             CREATE TABLE fs_dentry(id INTEGER PRIMARY KEY, name TEXT, parent_ino INTEGER, ino INTEGER);
             CREATE TABLE chunks(id INTEGER PRIMARY KEY, ino INTEGER, filepath TEXT);",
        )
        .unwrap();
        // S_IFREG = 0o100644 = 33188 ; a directory mode 0o40755 = 16877
        let reg = 33188;
        // ino 1: a.txt (indexed directly)            -> reachable
        // ino 2: b.xlsx (NOT indexed) + extracted.md  -> reachable via sibling
        // ino 3: c.pdf (NOT indexed, error sibling)   -> UNREACHABLE
        // ino 4: empty.txt size 0                     -> not content
        // ino 5: note.extracted.md (a sidecar)        -> not content
        // ino 6: b.xlsx.extracted.md (sibling, indexed)
        // ino 7: c.pdf.semfs-error.txt (sibling, NOT indexed)
        for (ino, mode, size) in [
            (1, reg, 100),
            (2, reg, 200),
            (3, reg, 300),
            (4, reg, 0),
            (5, reg, 50),
            (6, reg, 60),
            (7, reg, 70),
        ] {
            c.execute(
                "INSERT INTO fs_inode(ino,mode,size) VALUES(?,?,?)",
                [ino, mode, size],
            )
            .unwrap();
        }
        for (id, name, parent, ino) in [
            (1, "a.txt", 0, 1),
            (2, "b.xlsx", 0, 2),
            (3, "c.pdf", 0, 3),
            (4, "empty.txt", 0, 4),
            (5, "note.extracted.md", 0, 5),
            (6, "b.xlsx.extracted.md", 0, 6),
            (7, "c.pdf.semfs-error.txt", 0, 7),
        ] {
            c.execute(
                "INSERT INTO fs_dentry(id,name,parent_ino,ino) VALUES(?,?,?,?)",
                rusqlite::params![id, name, parent, ino],
            )
            .unwrap();
        }
        // chunks: ino 1 (a.txt) and ino 6 (b.xlsx.extracted.md) are indexed.
        c.execute("INSERT INTO chunks(ino,filepath) VALUES(1,'a.txt')", [])
            .unwrap();
        c.execute(
            "INSERT INTO chunks(ino,filepath) VALUES(6,'b.xlsx.extracted.md')",
            [],
        )
        .unwrap();
        c
    }

    #[test]
    fn seed_counts_classifies_sidecars_and_siblings() {
        let c = build_fixture();
        let (content, reachable, unreachable) = seed_counts(&c).unwrap();
        // content = a.txt, b.xlsx, c.pdf  (empty.txt excluded; the 3 sidecars excluded)
        assert_eq!(
            content, 3,
            "content files should exclude empties + sidecars"
        );
        // reachable = a.txt (direct) + b.xlsx (via .extracted.md sibling)
        assert_eq!(reachable, 2);
        assert_eq!(unreachable, vec!["c.pdf".to_string()]);
    }
}

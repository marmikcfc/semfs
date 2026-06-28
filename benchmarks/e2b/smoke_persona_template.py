#!/usr/bin/env python3
"""Infra smoke for a baked per-persona E2B template (Option B / SEM-37 DoD #4).

Boots `semfs-mount-{persona}` (built by benchmarks/modal/bake_e2b_persona.py) and
checks, on the REAL FUSE mount, that the baked assets actually work end-to-end at
the infrastructure level — BEFORE the heavier agent+judge E2E:

  1. baked assets present:  /opt/corpus.tgz + /opt/{persona}-gemma-q4.db
  2. corpus extracts:       /opt/corpus.tgz -> ~/ws/work/{persona}_standard (file count)
  3. seed is searchable:    mountless `semfs grep --tag` -> ranked hits (non-empty)
  4. FUSE mount works:      `semfs mount {persona}` -> root listing
  5. /kg/ overlay reads:    ls mnt/kg  (entities / communities surfaced from the KG)

No agent, no OpenRouter call — pure infra validation. Creds from .env (E2B_API_KEY).

Run:  set -a; . ./.env; set +a; python3 benchmarks/e2b/smoke_persona_template.py --persona kaifa
"""
import argparse
import json
import time

from e2b import Sandbox

# A generic discovery query per persona (kaifa = Backend Developer).
QUERIES = {
    "kaifa": "where is the project dependency / build configuration defined",
    "chanpin": "product requirements and roadmap",
    "houqin": "logistics shipment and inventory records",
    "yunying": "operations metrics and campaign performance",
}


def sh(sbx, cmd, timeout=180, env=None):
    r = sbx.commands.run(cmd, timeout=timeout, envs=(env or {}))
    return (r.stdout or ""), (r.stderr or "")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--persona", default="kaifa")
    ap.add_argument("--template", default="")
    ap.add_argument("--query", default="")
    args = ap.parse_args()

    persona = args.persona
    template = args.template or f"semfs-mount-{persona}"
    query = args.query or QUERIES.get(persona, "what is in this workspace")
    seed_opt = f"/opt/{persona}-gemma-q4.db"
    out = {"persona": persona, "template": template}

    print(f"booting {template} …", flush=True)
    sbx = Sandbox.create(template=template, timeout=1800)
    print("  sandbox:", sbx.sandbox_id, flush=True)
    try:
        # ── boot prep (mirror run_kaifa_smoke: link base assets, enable allow_other) ──
        sh(sbx, "mkdir -p ~/ws/work ~/.semfs && "
                "ln -sfn /opt/wb ~/wb && ln -sfn /opt/gemma_q4 ~/gemma_q4 && "
                "ln -sfn /opt/semfs-shims ~/semfs-shims 2>/dev/null; true", timeout=120)
        sh(sbx, "echo user_allow_other | sudo tee -a /etc/fuse.conf >/dev/null")

        # 1) baked assets present?
        o, _ = sh(sbx, f"ls -la {seed_opt} /opt/corpus.tgz 2>&1")
        out["baked_assets"] = o.strip()
        print("  [1] baked assets:\n" + o.strip(), flush=True)
        assert "No such file" not in o, "baked assets missing in template"

        # 2) corpus extracts cleanly?
        o, _ = sh(sbx, "tar xzf /opt/corpus.tgz -C ~/ws/work && "
                       "find ~/ws/work -type f | wc -l", timeout=600)
        out["corpus_files"] = o.strip().splitlines()[-1] if o.strip() else "0"
        print(f"  [2] corpus extracted: {out['corpus_files']} files", flush=True)

        # 3) mountless grep → ranked hits
        sh(sbx, f"cp {seed_opt} ~/.semfs/{persona}.db")
        marker = "\\n".join([
            f"container_tag={persona}", "api_url=https://api.supermemory.ai",
            "mount_path=/home/user/ws/work", f"db_path=/home/user/.semfs/{persona}.db",
            "backend=sqlite", "",
        ])
        senv = {"SEMFS_EMBED_MODEL": "gemma-q4", "SEMFS_EMBED_ONNX_DIR": "/home/user/gemma_q4",
                "SUPERMEMORY_API_KEY": "dummy-local", "SEMFS_NO_PUSH": "1",
                "SEMFS_NO_SYNC": "1", "SEMFS_SEARCH_ONLY": "on"}
        sh(sbx, f"printf '{marker}' > /home/user/ws/work/.semfs")
        o, e = sh(sbx, f'cd /home/user/ws/work && semfs grep --tag {persona} "{query}" 2>&1',
                  timeout=300, env=senv)
        grep_out = (o + e).strip()
        out["grep_query"] = query
        out["grep_tail"] = grep_out[-1500:]
        out["grep_has_hits"] = bool(grep_out) and "error" not in grep_out.lower()[:80]
        print(f"  [3] semfs grep (tail):\n{grep_out[-800:]}", flush=True)

        # 4) FUSE mount
        sh(sbx, "semfs unmount %s --force 2>/dev/null; fusermount -u /home/user/ws/mnt 2>/dev/null; "
                "mkdir -p /home/user/ws/mnt; sleep 1" % persona, timeout=60)
        o, e = sh(sbx, f"semfs mount {persona} --path /home/user/ws/mnt --backend fuse "
                       f"--key dummy-local --no-sync --no-push 2>&1 || true", timeout=300, env=senv)
        print(f"  [4] mount: {(o + e).strip()[-200:]}", flush=True)
        time.sleep(3)
        root, _ = sh(sbx, "ls /home/user/ws/mnt 2>&1 | head -20")
        out["mount_root"] = root.strip()
        print(f"  [4] mount root:\n{root.strip()}", flush=True)

        # 5) /kg/ overlay reads. The overlay is flat files (GRAPH_REPORT.md,
        # KNOWLEDGE_GRAPH.md, graph.json) — NOT a communities/ subdir.
        kg, _ = sh(sbx, "ls /home/user/ws/mnt/kg 2>&1")
        out["kg_overlay"] = kg.strip()
        kg_files = set(kg.split())
        out["kg_reads"] = bool({"graph.json", "KNOWLEDGE_GRAPH.md", "GRAPH_REPORT.md"} & kg_files)
        # content peek — prove the overlay is populated (entities/communities), not empty
        peek, _ = sh(sbx, "head -c 800 /home/user/ws/mnt/kg/KNOWLEDGE_GRAPH.md 2>/dev/null || "
                          "head -c 800 /home/user/ws/mnt/kg/GRAPH_REPORT.md 2>/dev/null")
        out["kg_peek"] = peek.strip()[:800]
        print(f"  [5] /kg/ overlay: {sorted(kg_files)}", flush=True)
        print(f"  [5] /kg/ content peek:\n{peek.strip()[:500]}", flush=True)

        out["VERDICT"] = "PASS" if (out["grep_has_hits"] and out["mount_root"] and out["kg_reads"]) else "CHECK"
        print("\n=== SMOKE SUMMARY ===")
        print(json.dumps({k: out[k] for k in
                          ("persona", "template", "corpus_files", "grep_has_hits",
                           "kg_reads", "VERDICT")}, indent=2))
    finally:
        try:
            sbx.kill()
        except Exception:
            pass
        print("sandbox killed.", flush=True)

    print("RESULT=" + json.dumps(out, ensure_ascii=False))


if __name__ == "__main__":
    main()

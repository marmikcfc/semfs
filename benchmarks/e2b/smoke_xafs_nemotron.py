#!/usr/bin/env python3
"""End-to-end smoke for the baked xafs E2B template, agent = Nemotron via OpenRouter.

Proves the FULL pipeline on ONE real xAFS question (dp_001/q01), on the REAL FUSE
mount, before scaling to the 110-question matrix:

  1. boot semfs-mount-xafs            (the baked template)
  2. baked assets present             (/opt/xafs-gemma-q4.db + /opt/corpus.tgz)
  3. FUSE mount the seed (ppr_on env: hidden-KG + co-mention + PPR diffusion)
  4. semfs grep the question          -> ranked context (PPR re-rank applied)
  5. answer with Nemotron (OpenRouter, WB_OR_MODEL) given the question + context
  6. judge the answer                 -> run_judge_xafs (Gemini 3.1 Pro, semantic match)

This is a "does it work" smoke (retrieval + LLM + judge), NOT the full agentic
tool-calling loop (that is the real run via run_matrix.py). It deliberately tests
the new pieces: the baked mount, Nemotron answering over semfs context, and the judge.

Caveat: the xafs seed indexes ALL 13 workspaces, so grep can surface cross-workspace
hits; the real run must scope retrieval per dp_XXX. For this dp_001-specific question
the relevant files rank high, which is enough to validate the plumbing.

Run:  set -a; . ./.env; set +a; python3 benchmarks/e2b/smoke_xafs_nemotron.py
"""
import argparse
import json
import os
import sys
import time
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import run_judge_xafs as J

from e2b import Sandbox

# Canonical smoke fixture (dp_001/q01 from supermemory/xAFS) — cases aren't baked,
# so the question + gold are supplied here.
FIXTURE = {
    "dp": "dp_001",
    "qid": "q01",
    "prompt": ("What was Coppertide's exact Stitch invoice amount for April 2026, "
               "as stated by Devansh Mehta on the kickoff call?"),
    "gold_answer": "$2,034",
}

# Seed must live in a USER-WRITABLE dir: semfs opens it RW (access-tracking + WAL/journal
# sidecars in the same dir). In root-owned /opt it fails "readonly database" → silently falls
# back to the cloud backend → 401 → zero results.
SEED_RW = "/home/user/.semfs/xafs.db"

MOUNT_ENV = {
    "SEMFS_EMBED_MODEL": "gemma-q4", "SEMFS_EMBED_ONNX_DIR": "/home/user/gemma_q4",
    "SUPERMEMORY_API_KEY": "dummy-local", "SEMFS_NO_PUSH": "1", "SEMFS_NO_SYNC": "1",
    "SEMFS_SEARCH_ONLY": "off",
    "SEMFS_RESULT_LIMIT": "5", "SEMFS_GREP_RESULT_CAP": "6144",
    "SEMFS_GREP_TOTAL_CAP": "10240", "SEMFS_REWRITE": "0",
    # ppr_on arm config (== hiddenkg_l7 + PPR diffusion)
    "SEMFS_KG": "off", "SEMFS_COMENTION": "on", "SEMFS_HIDDEN_KG": "on",
    "SEMFS_HIDDEN_KG_RETRIEVAL": "off", "SEMFS_KG_PPR": "on",
    "SEMFS_PPR_RESTART": "0.5", "SEMFS_PPR_ITERS": "30", "SEMFS_GRAPH_FS": "off",
}


def sh(sbx, cmd, timeout=180, env=None):
    r = sbx.commands.run(cmd, timeout=timeout, envs=(env or {}))
    return (r.stdout or ""), (r.stderr or "")


def nemotron_answer(question, context, model, api_key):
    """One OpenRouter completion: answer the question from the retrieved context."""
    body = json.dumps({
        "model": model, "temperature": 0,
        "messages": [
            {"role": "system", "content":
             "You answer questions about a personal file system using ONLY the provided "
             "retrieved context. Give a short, direct answer. If the context lacks the "
             "answer, say you cannot find it."},
            {"role": "user", "content":
             f"QUESTION:\n{question}\n\nRETRIEVED CONTEXT (semfs grep):\n{context[:12000]}\n\n"
             "Answer concisely:"},
        ],
    }).encode()
    req = urllib.request.Request(
        "https://openrouter.ai/api/v1/chat/completions", data=body,
        headers={"Authorization": f"Bearer {api_key}", "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=180) as resp:
        d = json.load(resp)
    return d["choices"][0]["message"]["content"], (d.get("usage") or {}).get("total_tokens", 0)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--template", default="semfs-mount-xafs")
    ap.add_argument("--model", default="nvidia/nemotron-3-ultra-550b-a55b:free")
    ap.add_argument("--seed", default="/opt/xafs-gemma-q4.db")
    args = ap.parse_args()
    orkey = os.environ["OPENROUTER_API_KEY"]
    out = {"template": args.template, "model": args.model, **{k: FIXTURE[k] for k in ("dp", "qid")}}

    print(f"booting {args.template} …", flush=True)
    sbx = Sandbox.create(template=args.template, timeout=1800)
    print("  sandbox:", sbx.sandbox_id, flush=True)
    try:
        sh(sbx, "mkdir -p ~/ws/work ~/.semfs && ln -sfn /opt/gemma_q4 ~/gemma_q4 2>/dev/null; true")
        sh(sbx, "echo user_allow_other | sudo tee -a /etc/fuse.conf >/dev/null")

        # 1) baked assets
        o, _ = sh(sbx, f"ls -la {args.seed} /opt/corpus.tgz 2>&1")
        out["baked_assets_ok"] = "No such file" not in o
        print("  [assets]\n" + o.strip(), flush=True)
        assert out["baked_assets_ok"], "baked assets missing"

        # 2) MOVE the baked seed into a user-writable dir (instant rename, same filesystem →
        #    no 5.9 GB copy, no disk blowup). In root-owned /opt semfs can't create its RW
        #    sidecars → "readonly database" → cloud fallback → 401. ~/.semfs is user-owned.
        sh(sbx, f"sudo mv {args.seed} {SEED_RW} && sudo chown user:user {SEED_RW} && "
                f"ls -la {SEED_RW}; df -h / | tail -1")
        marker = "\\n".join([
            "container_tag=xafs", "api_url=https://api.supermemory.ai",
            "mount_path=/home/user/ws/work", f"db_path={SEED_RW}",
            "backend=sqlite", "",
        ])
        sh(sbx, f"printf '{marker}' > /home/user/ws/work/.semfs")

        # 3) FUSE mount (prove the baked seed mounts in the real env)
        sh(sbx, "mkdir -p /home/user/ws/mnt; "
                "semfs unmount xafs --force 2>/dev/null; "
                "fusermount -u /home/user/ws/mnt 2>/dev/null; sleep 1", timeout=60)
        o, e = sh(sbx, "semfs mount xafs --path /home/user/ws/mnt --backend fuse "
                       "--key dummy-local --no-sync --no-push --startup-timeout 180 2>&1 || true",
                  timeout=300, env=MOUNT_ENV)
        time.sleep(3)
        root, _ = sh(sbx, "ls /home/user/ws/mnt 2>&1 | head")
        out["mount_root"] = root.strip()[:300]
        print(f"  [mount] root:\n{root.strip()[:300]}", flush=True)

        # 4) semfs grep → ranked context (PPR re-rank via MOUNT_ENV)
        o, e = sh(sbx, f'cd /home/user/ws/work && semfs grep --tag xafs "{FIXTURE["prompt"]}" 2>&1',
                  timeout=300, env=MOUNT_ENV)
        context = (o + e).strip()
        out["grep_chars"] = len(context)
        bad = any(s in context.lower() for s in
                  ("no results", "auth failed", "401", "readonly database", "error:", "falling back to cloud"))
        out["grep_has_hits"] = bool(context) and not bad
        print(f"  [grep] {out['grep_chars']} chars; tail:\n{context[-600:]}", flush=True)
        assert out["grep_has_hits"], "grep returned no hits"

        # 5) Nemotron answers from the retrieved context
        ans, atok = nemotron_answer(FIXTURE["prompt"], context, args.model, orkey)
        out["candidate_answer"] = ans.strip()
        out["agent_tokens"] = atok
        print(f"  [nemotron] tokens={atok} answer:\n{ans.strip()[:400]}", flush=True)

        # 6) Judge (Gemini 3.1 Pro, semantic match)
        v = J.grade_one(FIXTURE["prompt"], FIXTURE["gold_answer"], ans, api_key=orkey)
        out["correct"] = v["correct"]
        out["judge_reason"] = v["reason"]
        out["judge_tokens"] = v.get("judge_tokens")
        print(f"  [judge] correct={v['correct']} reason={v['reason']!r}", flush=True)

        out["VERDICT"] = "PASS" if (out["mount_root"] and out["grep_has_hits"]
                                    and out["correct"] is not None) else "CHECK"
    finally:
        try:
            sbx.kill()
        except Exception:
            pass
        print("sandbox killed.", flush=True)

    print("\n=== XAFS NEMOTRON SMOKE ===")
    print(json.dumps({k: out.get(k) for k in
                      ("template", "model", "dp", "qid", "mount_root", "grep_has_hits",
                       "candidate_answer", "correct", "judge_reason", "VERDICT")},
                     indent=2, ensure_ascii=False))
    print("RESULT=" + json.dumps(out, ensure_ascii=False))


if __name__ == "__main__":
    main()

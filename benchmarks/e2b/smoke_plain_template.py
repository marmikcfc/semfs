#!/usr/bin/env python3
"""Plain-arm smoke for a baked per-persona E2B template (SEM-39 gate).

Validates ONLY what the plain arm depends on at boot (run_matrix.boot_prep):
  1. /opt/corpus.tgz is baked (else boot_prep falls back to uploading CHANPIN's
     corpus → silent wrong-corpus for kaifa/houqin/yunying).
  2. The tarball's top-level dir is {persona}_standard — i.e. the RIGHT persona's
     corpus, not the chanpin fallback. THIS is the trap this smoke exists to catch.
  3. Corpus extracts and has a non-trivial file count.
  4. codex CLI + python3 + a ripgrep resolve (the codex/plain runtime contract).

No GPU, no mount, no seed, no agent call — pure plain-arm infra validation.
Creds from .env (E2B_API_KEY → the NEW account).

Run:  set -a; . ./.env; set +a; python3 benchmarks/e2b/smoke_plain_template.py --persona kaifa
"""
import argparse
import json

from e2b import Sandbox


def sh(sbx, cmd, timeout=300):
    r = sbx.commands.run(cmd, timeout=timeout)
    return (r.stdout or "") + (r.stderr or "")


def smoke(persona: str) -> dict:
    template = f"semfs-mount-{persona}"
    out = {"persona": persona, "template": template}
    print(f"booting {template} …", flush=True)
    sbx = Sandbox.create(template=template, timeout=900)
    print("  sandbox:", sbx.sandbox_id, flush=True)
    try:
        # 1) corpus baked?
        baked = sh(sbx, "test -f /opt/corpus.tgz && echo BAKED || echo NOBAKE").strip()
        out["corpus_baked"] = "BAKED" in baked

        # 2) right persona? top-level tar component must be {persona}_standard.
        top = sh(sbx, "tar tzf /opt/corpus.tgz 2>/dev/null | head -1").strip()
        out["corpus_top"] = top
        out["corpus_is_right_persona"] = top.split("/")[0] == f"{persona}_standard"

        # 3) extracts + file count
        n = sh(sbx, "rm -rf /tmp/p && mkdir -p /tmp/p && "
                    "tar xzf /opt/corpus.tgz -C /tmp/p --strip-components=1 2>/dev/null && "
                    "find /tmp/p -type f | wc -l", timeout=600).strip().splitlines()
        out["corpus_files"] = int(n[-1]) if n and n[-1].strip().isdigit() else 0

        # 4) codex/python/ripgrep runtime contract
        out["codex"] = sh(sbx, "codex --version 2>&1 | head -1").strip()
        out["python"] = sh(sbx, "python3 --version 2>&1 | head -1").strip()
        out["rg"] = sh(sbx, "( find /opt/wb -path '*ripgrep*linux*/rg' 2>/dev/null; "
                            "command -v rg ) | head -1").strip()
        out["codex_py"] = "OK" in sh(
            sbx, "test -f /opt/wb/evaluation/src/agents/codex.py && echo OK").strip()

        # case .task files — cell_driver.py:134 reads /opt/cases/<id>.task; an empty
        # /opt/cases is what crashed the first canary (FileNotFoundError, exit 1).
        nc = sh(sbx, "ls /opt/cases/*.task 2>/dev/null | wc -l").strip().splitlines()
        out["cases_baked"] = int(nc[-1]) if nc and nc[-1].strip().isdigit() else 0
        out["case_15_task"] = "OK" in sh(sbx, "test -f /opt/cases/15.task && echo OK").strip()

        out["VERDICT"] = "PASS" if (
            out["corpus_baked"] and out["corpus_is_right_persona"]
            and out["corpus_files"] > 0 and out["codex"].lower().startswith(("codex", "0", "1", "2"))
            and bool(out["codex"]) and out["codex_py"] and bool(out["rg"])
            and out["cases_baked"] > 0 and out["case_15_task"]
        ) else "FAIL"
    finally:
        try:
            sbx.kill()
        except Exception:
            pass
        print("  sandbox killed.", flush=True)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--persona", default="kaifa",
                    help="single persona, or 'all' for chanpin,kaifa,houqin,yunying")
    args = ap.parse_args()
    personas = (["chanpin", "kaifa", "houqin", "yunying"]
                if args.persona == "all" else [args.persona])
    results = []
    for p in personas:
        try:
            r = smoke(p)
        except Exception as ex:
            r = {"persona": p, "template": f"semfs-mount-{p}", "VERDICT": "ERROR", "err": repr(ex)[:300]}
        results.append(r)
        print(f"\n=== {p} ===\n" + json.dumps(r, indent=2), flush=True)
    print("\n=== SUMMARY ===")
    for r in results:
        print(f"  {r['persona']:<8} {r.get('VERDICT'):<6} "
              f"files={r.get('corpus_files','?')} top={r.get('corpus_top','?')}")
    ok = all(r.get("VERDICT") == "PASS" for r in results)
    print("ALL_PASS" if ok else "SOME_FAILED")


if __name__ == "__main__":
    main()

"""Kaifa-C dual-lane merge check (no LLM) — does `semfs grep` route to rrf_merge and fuse
BOTH lanes (LateOn-Code code + LFM2 doc)?

Boots np-kaifa-C, replicates setup_nextplaid for the merge case (extract both indices +
lfm2 + colgrep + semfs-np + rrf_merge + .semfs markers on both corpora + profile.d), then
runs the agent's exact pattern under `bash -lc` and checks the fused output carries
absolute paths from BOTH /srv/np/kaifa_code and /srv/np/kaifa_doc. ~5 min, ~free, NO GPU.

Run: python3 tickets/next-plaid-late-interaction/phase2_merge_check.py
"""
import pathlib
from e2b import Sandbox

REPO = pathlib.Path(__file__).resolve().parents[2]
SEMFS_SHIM = (REPO / "benchmarks/workspace_bench/semfs-shims/grep").read_text()
SEMFS_NP = (REPO / "tickets/next-plaid-late-interaction/semfs_np_wrapper.sh").read_text()
RRF = (REPO / "tickets/next-plaid-late-interaction/rrf_merge.py").read_text()
DOC = "/srv/np/kaifa_doc/corpus"
CODE = "/srv/np/kaifa_code/corpus"
ENV = {
    "SEMFS_BIN": "/usr/local/bin/semfs-np", "SEMFS_MOUNT_PATH": DOC,
    "SEMFS_REAL_HOME": "/home/user", "SEMFS_REAL_GREP": "/usr/bin/grep",
    "SEMFS_SHIM_DIR": "/home/user/semfs-shims", "SEMFS_SHIM_LOG": "/tmp/semfs-shim.log",
    "WB_NP_MERGE": "1", "WB_NP_CORPUS": DOC,
    "NP_CODE_DIR": CODE, "NP_CODE_MODEL": "lightonai/LateOn-Code", "NP_CODE_XDG": "/srv/np/kaifa_code/_xdg",
    "NP_DOC_DIR": DOC, "NP_DOC_MODEL": "/lfm2/lfm2-colbert-350m-onnx", "NP_DOC_XDG": "/srv/np/kaifa_doc/_xdg",
}


def sh(sbx, cmd, timeout=400, env=None):
    r = sbx.commands.run(cmd, timeout=timeout, envs=(env or {}))
    return (r.stdout or "") + (r.stderr or "")


print("creating sandbox from np-kaifa-C …", flush=True)
sbx = Sandbox.create(template="np-kaifa-C", timeout=1200)
try:
    sbx.files.write("/tmp/grep_shim", SEMFS_SHIM)
    sbx.files.write("/tmp/semfs-np", SEMFS_NP)
    sbx.files.write("/tmp/rrf.py", RRF)
    print(sh(sbx, "sudo mkdir -p /opt/semfs-shims /srv/np /lfm2 /opt/np && "
                  "sudo cp /tmp/grep_shim /opt/semfs-shims/grep && sudo chmod +x /opt/semfs-shims/grep && "
                  "sudo cp /tmp/semfs-np /usr/local/bin/semfs-np && sudo chmod +x /usr/local/bin/semfs-np && "
                  "sudo cp /tmp/rrf.py /opt/np/rrf_merge.py && "
                  "sudo tar xzf /opt/np/kaifa_code.tgz -C /srv/np && sudo tar xzf /opt/np/kaifa_doc.tgz -C /srv/np && "
                  "sudo chmod -R a+rwX /srv/np && "
                  "sudo cp /opt/np/colgrep /usr/local/bin/colgrep && sudo chmod +x /usr/local/bin/colgrep && "
                  "{ test -d /lfm2/lfm2-colbert-350m-onnx || sudo tar xzf /opt/np/lfm2.tgz -C /lfm2; } && "
                  "df -h / | tail -1 && "
                  f"printf 'mount_path=%s\\n' '{DOC}' | sudo tee {DOC}/.semfs >/dev/null && "
                  f"printf 'mount_path=%s\\n' '{CODE}' | sudo tee {CODE}/.semfs >/dev/null && "
                  "echo 'export PATH=/opt/semfs-shims:$PATH' | sudo tee /etc/profile.d/00-np-shims.sh >/dev/null && "
                  "echo SETUP_DONE", timeout=900), flush=True)

    print("\n=== 1) bash -lc resolves grep to the shim? ===", flush=True)
    print(sh(sbx, "bash -lc 'command -v grep'", env=ENV), flush=True)

    print("\n=== 2) warm LateOn-Code (code lane) — first HF pull ===", flush=True)
    print(sh(sbx, f"cd {CODE} && XDG_DATA_HOME=/srv/np/kaifa_code/_xdg colgrep --model lightonai/LateOn-Code "
                  "--json 'authentication' 2>&1 | head -c 200 || true", timeout=600), flush=True)

    print("\n=== 3) agent-style merge query → rrf_merge fuses both lanes? ===", flush=True)
    out = sh(sbx, "bash -lc 'grep \"user authentication login\" " + DOC + "'", env=ENV, timeout=600)
    print(out[:1800], flush=True)

    print("\n=== 4) shim routing log ===", flush=True)
    print(sh(sbx, "cat /tmp/semfs-shim.log 2>/dev/null | tail -5"), flush=True)

    has_code = "/srv/np/kaifa_code/" in out
    has_doc = "/srv/np/kaifa_doc/" in out
    print(f"\n=== VERDICT: paths from code-lane={has_code}  doc-lane={has_doc}  "
          f"BOTH(merge works)={has_code and has_doc} ===", flush=True)
finally:
    sbx.kill()
    print("sandbox killed", flush=True)

"""Quick routing check (no LLM) — does the agent's `grep` route to colgrep via semfs grep?

Boots np-houqin-all, replicates setup_nextplaid (extract index + colgrep + semfs-np + .semfs
marker + profile.d shim-PATH), then runs the agent's exact pattern under `bash -lc` and checks
whether colgrep fired (ranked output / routing log) vs plain grep. ~2 min, ~free.

Run: python3 tickets/next-plaid-late-interaction/phase2_routing_check.py
"""
import pathlib
from e2b import Sandbox

REPO = pathlib.Path(__file__).resolve().parents[2]
SEMFS_SHIM = (REPO / "benchmarks/workspace_bench/semfs-shims/grep").read_text()
SEMFS_NP = (REPO / "tickets/next-plaid-late-interaction/semfs_np_wrapper.sh").read_text()
CORPUS = "/srv/np/houqin_all/corpus"
XDG = "/srv/np/houqin_all/_xdg"
MODEL = "/lfm2/lfm2-colbert-350m-onnx"
NPENV = {"SEMFS_BIN": "/usr/local/bin/semfs-np", "SEMFS_MOUNT_PATH": CORPUS,
         "SEMFS_REAL_HOME": "/home/user", "SEMFS_REAL_GREP": "/usr/bin/grep",
         "SEMFS_SHIM_DIR": "/home/user/semfs-shims",
         "WB_NP_CORPUS": CORPUS, "WB_NP_MODEL": MODEL, "XDG_DATA_HOME": XDG,
         "SEMFS_SHIM_LOG": "/tmp/semfs-shim.log"}


def sh(sbx, cmd, timeout=300, env=None):
    r = sbx.commands.run(cmd, timeout=timeout, envs=(env or {}))
    return (r.stdout or "") + (r.stderr or "")


print("creating sandbox from np-houqin-all …", flush=True)
sbx = Sandbox.create(template="np-houqin-all", timeout=900)
try:
    # real semfs grep shim (executable) + np setup
    sbx.files.write("/tmp/grep_shim", SEMFS_SHIM)
    sbx.files.write("/tmp/semfs-np", SEMFS_NP)
    print(sh(sbx, "sudo mkdir -p /opt/semfs-shims /srv/np /lfm2 /opt/np && "
                  "sudo cp /tmp/grep_shim /opt/semfs-shims/grep && sudo chmod +x /opt/semfs-shims/grep && "
                  "sudo cp /tmp/semfs-np /usr/local/bin/semfs-np && sudo chmod +x /usr/local/bin/semfs-np && "
                  "sudo tar xzf /opt/np/houqin_all.tgz -C /srv/np && sudo chmod -R a+rwX /srv/np && "
                  "sudo cp /opt/np/colgrep /usr/local/bin/colgrep && sudo chmod +x /usr/local/bin/colgrep && "
                  "sudo tar xzf /opt/np/lfm2.tgz -C /lfm2 && sudo chmod -R a+rX /lfm2 && "
                  f"printf 'mount_path=%s\\n' '{CORPUS}' | sudo tee {CORPUS}/.semfs >/dev/null && "
                  "echo 'export PATH=/opt/semfs-shims:$PATH' | sudo tee /etc/profile.d/00-np-shims.sh >/dev/null && "
                  "echo SETUP_DONE", timeout=600), flush=True)

    print("\n=== 1) does `bash -lc` resolve grep to the shim? ===", flush=True)
    print(sh(sbx, "bash -lc 'command -v grep; echo PATH=$PATH'", env=NPENV), flush=True)

    print("\n=== 2) agent-style: grep \"胜业电气\" <corpus>  → routes to colgrep? ===", flush=True)
    out = sh(sbx, "bash -lc 'grep \"胜业电气\" " + CORPUS + "'", env=NPENV)
    print(out[:1500], flush=True)

    print("\n=== 3) shim routing log ===", flush=True)
    print(sh(sbx, "cat /tmp/semfs-shim.log 2>/dev/null | tail -8"), flush=True)

    routed = ("ROUTING grep to semfs" in out) or ("Model:" in out) or ("📂" in out) or (".extracted.md" in out and "score" in out.lower())
    print(f"\n=== VERDICT: colgrep routed = {routed} ===", flush=True)
finally:
    sbx.kill()
    print("sandbox killed", flush=True)

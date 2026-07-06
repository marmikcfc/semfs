# Harbor FUSE-mount patch — run the semfs arms on the real sftpgo image (EC2)

**Goal.** Reuse the *proven plain-run footing* (EC2 · harbor docker · claude-code → Ornith-35B ·
deepseek judge) and add the one missing piece: mount the **gliner-KG seed** in the task container so
the agent retrieves via **`semfs grep`** under a per-arm `SEMFS_KG_PPR` setting. This is the 5th harbor
patch (after: Modal entrypoint-clear · Daytona os_user=root · Daytona bake · claude-code Bearer-auth).

Seed: `sftpgo-gliner.db` from `build_sftpgo_seed_ec2.sh` (chunks+vectors + AST/tree-sitter code KG +
gliner doc KG + Leiden communities → all arms covered).

---

## The arms (env-flag sets, from `benchmarks/e2b/run_matrix.py`)

| arm | search tool | env |
|---|---|---|
| **plain** | ripgrep over `/app` | — (baseline, = the completed plain run) |
| **ppr_off** | `semfs grep` | `SEMFS_HIDDEN_KG=on SEMFS_COMENTION=on SEMFS_HIDDEN_KG_RETRIEVAL=off SEMFS_KG_PPR=off` |
| **ppr_on** | `semfs grep` | …same… `SEMFS_KG_PPR=on` |
| **ppr_map** | `semfs grep` | ppr_on **+** `WB_WORKSPACE_MAP=<map.txt>` (prepended to the agent prompt) |

`SEMFS_KG=off` in all semfs arms (surface KG off; the prior is the hidden-KG path). ppr_map's map is
generated from the seed by `benchmarks/e2b/semfs_map.py` (FS skeleton + Leiden-community overlay).

---

## Two mechanisms this patch must add

### 1. FUSE in the container (compose caps)
Extend the patched `harbor/environments/docker/docker-compose-prebuilt.yaml`:

```yaml
services:
  main:
    image: ${PREBUILT_IMAGE_NAME}
    entrypoint: []
    command: [ "sh", "-c", "sleep infinity" ]
    cap_add: [ SYS_ADMIN ]                 # + FUSE
    devices: [ "/dev/fuse" ]
    security_opt: [ "apparmor:unconfined" ]
```
Host prereq on the EC2 box: `apt-get install -y fuse3` and `/dev/fuse` present (Ubuntu-24.04 AppArmor
caveat — see rcas/2026-06-09). Equivalently, populate the run-config `environment.extra_docker_compose`
list instead of editing the template (verify harbor merges it — that code is in the external fork).

### 2. `semfs grep` as the agent's search tool  ⚠️ the load-bearing part
**A prompt hint does NOT work for claude-code** — it ignores a "use `semfs grep`" hint
(memory `semfs-claude-affordance`: fix = a real tool/shim, not a hint). So the agent-setup phase
(before `claude --print`, in `harbor/agents/installed/claude_code.py`) must, for semfs arms:

```sh
# deliver binary + seed (baked into a semfs layer, or docker cp / volume)
install -m755 semfs /usr/local/bin/semfs
mkdir -p ~/.semfs && cp sftpgo-gliner.db ~/.semfs/sftpgo.db
# mount the seed  — SIDECAR (recommended): keep /app the REAL runnable repo.
# --no-import is CRITICAL: mounting DEFAULTS to import+index the mountpoint's files
# (import_existing = !--no-import), and with a gliner-kg daemon that RE-BUILDS the KG
# LIVE — dropping the batch Leiden communities (ppr_map breaks) + non-deterministic.
# An empty sidecar path has nothing to import anyway; --no-import is belt-and-suspenders
# so the pre-built batch seed (KG + communities) is served UNTOUCHED.
# ⚠️ SEMFS_EMBED_MODEL=gemma is MANDATORY and load-bearing: seed_dir builds the seed
# with EmbeddingGemma300M (768d), but the daemon/grep DEFAULT to multilingual-e5-small
# (384d). Mismatch → daemon logs "local index disabled" → grep silently falls back to
# cloud → "no local index" → the arm scores like plain. Set it on BOTH mount AND every
# `semfs grep` (grep embeds the query in its own process). Verified locally: with it,
# grep returns sftpd.go top for a command-injection query; without it, every arm fails.
export SEMFS_EMBED_BACKEND=local SEMFS_EMBED_MODEL=gemma
semfs mount sftpgo --path /semfs --no-push --no-sync --no-import
printf 'mount_path=/semfs\n' > /semfs/.semfs          # marks it so `semfs grep` routes here
# (agent's `semfs grep` invocations must ALSO see SEMFS_EMBED_MODEL=gemma)
# route the agent's search → semfs grep (shim/override, like run_matrix's ClaudeCode.js override)
#   + export the arm env (SEMFS_KG_PPR etc.)
export SEMFS_HIDDEN_KG=on SEMFS_COMENTION=on SEMFS_HIDDEN_KG_RETRIEVAL=off SEMFS_KG_PPR=<off|on>
```

---

## Mount location — the one real design fork

sftpgo QnA tasks can require **executing** the server. A semfs mount is a *materialized-from-SQLite*
view, so mounting **over `/app`** can break `go build`/exec (and `SEMFS_SEARCH_ONLY` hides files from a
tree-walk). Two options:

- **SIDECAR (recommended):** mount at `/semfs`; `/app` stays the real runnable repo. Agent *executes*
  in `/app`, *searches* via `semfs grep` (routes to the `/semfs` seed). Clean A/B (only the search tool +
  ppr change vs plain), no exec risk. Needs the grep-shim to target the seed regardless of cwd.
- **OVER `/app`:** purest mount test, but requires `SEMFS_SEARCH_ONLY=off` **and** empirical proof the
  built server still runs through FUSE. Only if a task truly needs the semantic view *as* the workspace.

→ **Start sidecar.** Reconsider over-`/app` only if the arms need the mount to BE the workspace.

---

## Delivery of binary + seed + model into the container
Mirror the existing "Daytona bake" pattern (`Image.base(img).run_commands(...)`): bake a thin layer with
`semfs` (linux, built with `gliner-kg`), `~/.semfs/sftpgo.db`, and — only if live re-indexing is ever
used — the gliner model. For the pre-built seed path, the **model is not needed at run time** (KG is
already in the seed); only the `semfs` binary + the seed. Caches per image → near-zero per-task setup.

---

## Open items to finalize ON the EC2 box (needs the harbor source)
1. Confirm harbor merges `extra_docker_compose` into the rendered compose (vs editing the template).
2. Wire the **grep-shim / MCP tool** so claude-code's search actually calls `semfs grep` (the run_matrix
   `ClaudeCode.js` override is the reference).
3. Generate the ppr_map `map.txt` via `semfs_map.py` from the seed; prepend per the run config.
4. Reuse the plain run's `plain_docker_ornith.yaml` as the base; fork one config per arm (env only).

## Smoke → matrix
1. **Smoke:** 1 sftpgo task, arm=ppr_off (sidecar mount + `semfs grep`) vs plain → confirm the whole
   chain (mount → grep routes → Ornith answers → deepseek judges).
2. **Matrix:** `{plain, ppr_off, ppr_on, ppr_map}` × 7 sftpgo tasks, `-k` rollouts, deepseek judge.
   Compare reward/agg + output tokens, paired, vs the plain baseline (3/7 solved).

**Gate:** the Ornith-35B agent serve is a Modal GPU (like the plain run) — needs explicit go-ahead
(standing rule). The seed build itself is GPU-free.

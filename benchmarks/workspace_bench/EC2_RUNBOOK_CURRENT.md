# EC2 Runbook — CURRENT method (inside-the-mount, product-only)

_Last updated: 2026-06-08. **Supersedes** the testing flow in `SEMFS_TESTING_RUNBOOK.md`
(05-28), `EC2_TESTING_PROGRESS.md` (06-01), `SEMFS_BENCHMARK_RUNBOOK.md` (05-27) — those
describe the older 4-variant harness with `_SEMFS_PROTOCOL` coaching and path-existence
scoring. This doc describes how we test **now**. Companion: root `EXPERIMENTS.md`,
`CURRENT_STATE.md`._

---

## 0. What changed vs the old runbooks (read first)

| | OLD method | **CURRENT method (this doc)** |
|---|---|---|
| Agent guidance | `_SEMFS_PROTOCOL` prepended by the harness (coaching) | **none** — `_SEMFS_PROTOCOL` REMOVED. Product delivers via injected `AGENTS.md` + the `grep` shadow only |
| Where codex runs | mount, but coached | **inside the mount, product-only** (`--cd <mount>`, identical upstream prompt as baseline) |
| Retrieval steering | protocol "use semfs grep" | **`grep` shell shadow** (flagless `grep` in a mount → `semfs grep`) + injected `AGENTS.md` |
| Scoring | mostly `status` = path-exists | **real rubric judge: Seed-2.0-Lite** |
| KG | `/KNOWLEDGE_GRAPH.md` at root | **`/kg/` folder** (`KNOWLEDGE_GRAPH.md`, `GRAPH_REPORT.md`, `graph.json`) + root `AGENTS.md`/`CLAUDE.md` |

**Principle:** the semfs arm must send the *identical* prompt the baseline gets. Any help
comes from the **product** (mount + injected `AGENTS.md` + semantic-`grep`), never from
harness-only prompt text. That keeps the A/B honest (no benchmark gaming).

---

## 1. Access

```bash
S="ssh -i ~/.ssh/semfs-benchmark -o ConnectTimeout=20 ubuntu@13.201.35.159"
$S 'hostname'        # ip-172-31-46-24
```
- EC2 `m7i.xlarge` (4 vCPU / 16 GB, no GPU), `ap-south-1`, IP `13.201.35.159`.
- **PATH gotcha:** `semfs` is on the **login**-shell PATH only. Always either
  `$S 'bash -lc "…"'` **or** use the full path `/home/ubuntu/.local/bin/semfs`.
- Repo on box (rsync, NOT git): `/srv/semfs-benchmark/semantic-filesystem`.
- Binary the harness uses: `/home/ubuntu/.local/bin/semfs`.
- Mounts open `~/.semfs/<tag>.db` (NOT `XDG_CACHE_HOME`). Active tag: `chanpin-e5-nosum`.
- Secrets: `/home/ubuntu/.semfs_seed_env` (OpenRouter key). **Never print keys**
  (`${VAR:+SET}` only).

---

## 2. Deploy a code change (local → box → binary)

```bash
K=~/.ssh/semfs-benchmark; H=ubuntu@13.201.35.159
DST=/srv/semfs-benchmark/semantic-filesystem
# 1. sync changed source (Rust + harness)
rsync -az -e "ssh -i $K" crates/ $H:$DST/crates/
rsync -az -e "ssh -i $K" benchmarks/workspace_bench/semfscodex.py $H:$DST/benchmarks/workspace_bench/
# 2. release build on box (background + poll BUILD_EXIT)
$S "cd $DST && nohup bash -lc 'export PATH=\$HOME/.cargo/bin:\$PATH; cargo build --release -p semfs-core -p semfs > /tmp/build.log 2>&1; echo BUILD_EXIT=\$? >> /tmp/build.log' >/dev/null 2>&1 & echo started"
$S 'tail -1 /tmp/build.log'   # until BUILD_EXIT=0
# 3. install the binary where the harness looks
$S "install -m755 $DST/target/release/semfs /home/ubuntu/.local/bin/semfs && /home/ubuntu/.local/bin/semfs --version"
# 4. (re)install the grep shadow into zsh + bash + login shells
$S '/home/ubuntu/.local/bin/semfs init'
```

`agent_hint` (injected `AGENTS.md`/`CLAUDE.md`) and the `/kg/` artifacts are written
automatically at **mount** time — no separate step.

---

## 3. One-time / per-tag setup

- **KG built on the tag's db.** The KG (communities, god-nodes, typed relations) is built
  on `~/.semfs/chanpin-e5-nosum.db`. Rebuild from scratch with
  `examples/build_kg.rs` (see `crates/semfs-core/examples/build_kg.rs`):
  `OPENROUTER_API_KEY=… cargo run --release -p semfs-core --example build_kg -- ~/.semfs/chanpin-e5-nosum.db`
  (current state: 9,146 entities, 4,783 relations).
- **Judge config** `/tmp/judge_seed.yaml` — the paper's judge:
  ```yaml
  model_name: "seed-2.0-lite-judge"
  baseUrl:    "https://openrouter.ai/api/v1"
  model:      "bytedance-seed/seed-2.0-lite"
  apiKey:     "<OPENROUTER_API_KEY>"   # ⚠ DO NOT hardcode — see §7; read from env/secrets
  ```

---

## 4. Run one case (the kg-series loop)

The driver is `run289.sh <tag> <embed> <mode> <stamp>` → writes a trace to
`/tmp/trace_<stamp>`. `mode` ∈ `kg_on | kg_off | cloud`.

`run289.sh` sets (the current config):
```
SEMFS_BIN=/home/ubuntu/.local/bin/semfs
XDG_CACHE_HOME=/srv/semfs-benchmark/rewrite-test/cache
SEMFS_REWRITE=1 SEMFS_RETURN_MODE=snippet SEMFS_RESULT_LIMIT=2 SEMFS_SEARCH_ONLY=on
SEMFS_MOUNT_TIMEOUT_SEC=900 SEMFS_STARTUP_TIMEOUT_SEC=600
DATASET=smoke RUN_STAMP=<stamp>
# mode=kg_on  → SEMFS_NO_PUSH=1 SEMFS_NO_SYNC=1 SEMFS_KG=on   (local, product KG on)
# mode=kg_off → … SEMFS_KG=off   (A/B baseline: no KG)
# mode=cloud  → SEMFS_STORAGE_BACKEND=cloud SEMFS_NO_PUSH=1
```
It cleans prior mounts (specific tags, no pattern-kill), mounts, runs the harness
(`benchmarks/aws/run_workspace_bench.sh` → upstream `agent_runner.py`), stages outputs,
unmounts.

**`kgrun.sh`** wraps a full single-case run + judge (edit `stamp=` per run):
```bash
$S 'bash /tmp/kgrun.sh' >/tmp/<stamp>_run.log 2>&1   # ~6–8 min
```
It runs `run289.sh chanpin-e5-nosum e5-small kg_on <stamp>`, prints telemetry, then judges
(see §6) and stores `/tmp/judge_<stamp>.json`.

> ⚠ The semfs arm now sends the **identical upstream-wrapped prompt** as the baseline —
> `benchmarks/workspace_bench/semfscodex.py` no longer prepends `_SEMFS_PROTOCOL`. Do not
> reintroduce it (that was harness coaching = gaming).

---

## 5. Verify the SIGNAL, not the invocation (mandatory checks)

| Question | ✅ Proof |
|---|---|
| Is the `grep` shadow active for codex's shell? | `$S 'bash -lc "type grep"'` → **`grep is a function`**; inside a mount, flagless `grep "q"` returns `# supermemory semantic search …` |
| Is the product `AGENTS.md` actually read by codex? | **canary:** put a token in the mount-root `AGENTS.md`, run codex against an echo server, grep the captured request (see `/tmp/codex_canary.sh`). Confirmed: token in **6/6** model calls. |
| Did the agent use semantic search? | `semfs grep` in the trace (`cmd_seq.py`), not just an API count |
| Did the 403 reach the agent? | `saw403=1` in `parse289.py` output |
| Did the agent REPORT it (the real test)? | grep the **deliverable** file for `403`/`Forbidden` — NOT the tool log. (Known gap: agent sees it, omits it.) |
| What did it cost? | all usage fields; note **`cached_input_tokens=0`** on this proxy → tokens ≈ per-turn context × turns (turn count is the driver) |

---

## 6. Score it (Seed-2.0-Lite rubric judge)

```bash
$S 'bash -lc "
  EVAL=/srv/semfs-benchmark/Workspace-Bench/evaluation
  OUT=\$EVAL/output/SEMFSCodex--GPT-5.4--Smoke-SEMFS/289
  cd \$EVAL
  timeout 300 python3 src/agent_eval.py --task-dir \"\$OUT\" --eval-yaml /tmp/judge_seed.yaml --overwrite
  cp \"\$OUT/rubrics_judge--seed-2.0-lite-judge.json\" /tmp/judge_<stamp>.json
"'
# read it: python3 /tmp/showjudge.py /tmp/judge_<stamp>.json   → summary {total,passed,failed} + per-rubric evidence
```
- Metric = **rubric pass rate** (`summary.passed / total`). NOT `status` (path-existence).
- Case 289 ceiling ≈ **10/15** ([5][6] path-convention + [8][9][10] metadata meta-task are
  structurally unwinnable in this config; [1][2][3][7][13][14] are the honesty rubrics).
- If the judge returns "Judge output parse failed" for all rubrics, that's a judge infra
  error (malformed model output) — **re-run the judge**, it's not a real 0/15.

---

## 7. Helper scripts (currently in `/tmp` on box — SHOULD be committed)

| script | purpose |
|---|---|
| `run289.sh` | run one case for a tag/embed/mode/stamp → `/tmp/trace_<stamp>` |
| `kgrun.sh` | run289 (kg_on) + telemetry + seed judge + store judge json |
| `parse289.py` | telemetry: tokens, tool_calls, os.walk/grep counts, out_bytes |
| `cmd_seq.py` | ordered shell-command sequence codex executed |
| `showjudge.py` | pretty-print a `rubrics_judge*.json` (summary + per-rubric) |
| `judge_seed.yaml` | Seed-2.0-Lite judge config |
| `codex_canary.sh` | echo-server canary that proves `AGENTS.md` reaches the model |

These live in ephemeral `/tmp` — a reboot loses them. **TODO: copy into
`benchmarks/workspace_bench/` and git-track** so the kg-series is reproducible.

---

## 8. Security / ops (standing)

- **Never print API keys.** `judge_seed.yaml` currently **hardcodes** an OpenRouter key —
  this is a leak; **rotate it** and change the file to read `apiKey` from an env var /
  `/home/ubuntu/.semfs_seed_env`. Same for any `--key` on daemon command lines.
- Do **not** reboot the EC2 instance without explicit OK. Keep all seeds intact.
- Mount cleanup: unmount specific tags (`semfs unmount <tag>`); never pattern-kill.

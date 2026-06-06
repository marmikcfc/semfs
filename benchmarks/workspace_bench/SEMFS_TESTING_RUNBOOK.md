# semfs — Testing Runbook

**Purpose.** Reproducible procedures to **run** the semfs benchmark experiments and, crucially, **verify** them — including the things that fooled us (delivery, token accounting, supermemory-call attribution). If you only read one rule: **a test isn't passed because a command ran; it's passed because you verified the signal that proves it.**

**Date:** 2026-05-28. Host: EC2 `m7i.xlarge` `i-0c491c7cc23de8555` (`ap-south-1`), `ubuntu@13.201.35.159`, key `~/.ssh/semfs-benchmark`.

---

## 0. The golden rule (learned the hard way)

> **Verify the *signal*, not the *invocation*.** Setting an option, seeing a command run, or grepping a log are NOT proof. We spent runs 4–10 believing a hint was delivered (we'd "set the option") when the model never received it. The proofs that actually count are below; use them.

| Question | ❌ NOT proof | ✅ Proof |
|---|---|---|
| Did the agent get the hint? | "we set `appendSystemPrompt`" / stderr breadcrumb | **`[SEMFS-ACK]` canary in the transcript** |
| Did semfs's semantic search run? | grep the daemon log | **the Supermemory dashboard** (logs don't record `/v4/*` at INFO) |
| What did it cost? | `input+output` tokens | **all four usage fields** (incl. `cache_read`/`cache_write`) |
| Did the agent use semfs? | API call count (warm cache → 0) | **`semfs grep` in transcript** + **R2 rehydrations** in the daemon log |

---

## 1. Prerequisites (check once per session)

```bash
S="ssh -i ~/.ssh/semfs-benchmark -o ConnectTimeout=20 ubuntu@13.201.35.159"
# NOTE: semfs is only on the LOGIN-shell PATH → always use:  $S 'bash -lc "…"'
# NOTE: never put ( ) in a remote `bash -lc "echo === … ==="` — parens break the shell.

$S 'bash -lc "
  semfs --version                       # binary present
  test -f ~/.config/semfs/credentials.json && echo creds-OK
  command -v semfs                       # → /home/ubuntu/.local/bin/semfs
  command -v rg grep                     # rg NOT installed (shim provides it); grep = /usr/bin/grep
  nproc; free -h | awk \"/Mem:/{print \\\$2}\"; df -h / | tail -1   # 4 vCPU, ~16G, disk free
"'
```
- **Container seeded?** The benchmark reads a *per-workspace* Supermemory container (`workspace-bench-<persona>`). `semfs list` shows only active *mounts*, not the server-side seed — seeding is verified by a successful `semfs grep` returning results, or on the dashboard.
- **Auth:** Codex → OpenRouter (`benchmark.env`); Claude → `USE_CLAUDE_LONG_RUNNING_TOKEN=1` (OAuth subscription).

---

## 1.5 Dataset modes & the smoke test

`DATASET` controls how many cases a run executes:

| `DATASET` | scope | `task_limit` | use for |
|---|---|---|---|
| **`smoke`** | the **single** case in `tasks_lite/` (we pinned it to **289**) | 1 | fast end-to-end sanity — **every test T1–T8 runs in smoke mode** |
| `lite` | the 100-task Lite subset | 100 | a real (small) benchmark across personas |
| `full` | all 388 tasks | 388 | the full benchmark |

> **All procedures in this runbook are smoke runs** (`DATASET=smoke`, one case, ~10–15 min for claude). Smoke is the unit of testing here: it exercises the *entire* pipeline (config → prepare → mount → agent → grade) on one case, so it's the right thing to run first and the right thing to iterate on. Only move to `lite`/`full` once smoke is green and you want statistical signal.

### The 60-second smoke test (run this BEFORE anything else)
Confirms the harness, mount, auth, and grading all work — *before* you debug a specific variant.
```bash
$S 'bash -lc "
  cd /srv/semfs-benchmark
  rm -rf Workspace-Bench/evaluation/output/Codex--GPT-5.4--Smoke/289   # avoid stale-resume
  setsid env DATASET=smoke RUN_STAMP=smoke \
    /srv/semfs-benchmark/semantic-filesystem/benchmarks/aws/run_workspace_bench.sh codex \
    > /tmp/smoke.log 2>&1 < /dev/null & echo launched"'
# then poll for output/Codex--GPT-5.4--Smoke/289/agent.json → status:passed
```
- **Why plain `codex` for the smoke test:** it's the cheapest path (no mount, no seed dependency, OpenRouter auth) — if *this* fails, the problem is the harness/env, not semfs. Once it passes, escalate to `semfs-codex` (adds mount+search), then the claude variants (add OAuth + delivery).
- **Smoke ≠ semfs working.** A green smoke on plain `codex` only proves the *pipeline*; semfs behavior is proven by T2/T4's signals (`semfs grep`, `[SEMFS-ACK]`, dashboard), not by `status:passed`.

---

## 2. Test catalog (what to run and how to know it passed)

| ID | Test | Command target | PASS criteria |
|---|---|---|---|
| **T1** | plain codex baseline | `codex` | `status=passed`; dashboard shows **0** `/v4/*`; no `semfs grep` |
| **T2** | semfs-codex | `semfs-codex` | `status=passed`; dashboard `profile_v4`+`search_v4`; `semfs grep` in transcript; tokens **< plain codex** (within-model) |
| **T3** | plain claude | `claudecode` | `status=passed`; dashboard **0** `/v4/*` |
| **T4** | semfs-claude delivery | `semfs-claudecode` | **`[SEMFS-ACK]` in transcript** (read) **and** `semfs grep` used (complied) |
| **T5** | token accounting | any claude run | usage has non-zero `cache_read`/`cache_write`; `total = input+cache_write+cache_read+output` |
| **T6** | shim interception | semfs-claude + shim | with `USE_BUILTIN_RIPGREP=0`, `/tmp/semfs-shim.log` shows `rg argv:` (native Grep hit the shim) |
| **T7** | attribution | all 4 | Supermemory touched **only** in semfs variants |
| **T8** | clean isolation | `SEMFS_FRESH=1` | prior cache DB + output dir removed before the run |

---

## 3. Running a test

### 3.1 Launch (detached, survives SSH drops)
```bash
$S 'bash -lc "
  cd /srv/semfs-benchmark
  # clear stale output so the grader does not SKIP the case (stale-resume):
  rm -rf Workspace-Bench/evaluation/output/<LABEL>/289
  rm -f /tmp/<run>.log /tmp/semfs-shim.log
  setsid env DATASET=smoke RUN_STAMP=<run> \
    USE_CLAUDE_LONG_RUNNING_TOKEN=1 \
    SEMFS_CONTAINER_TAG=workspace-bench-chanpin SEMFS_NO_PUSH=1 SEMFS_MOUNT_TIMEOUT_SEC=900 \
    /srv/semfs-benchmark/semantic-filesystem/benchmarks/aws/run_workspace_bench.sh <target> \
    > /tmp/<run>.log 2>&1 < /dev/null &
  sleep 4; pgrep -af run_workspace_bench.sh | grep -v pgrep | head -1
"'
```
- `<target>` ∈ `codex | semfs-codex | claudecode | semfs-claudecode`.
- `<LABEL>` output dirs: `Codex--GPT-5.4--Smoke` · `SEMFSCodex--GPT-5.4--Smoke-SEMFS` · `ClaudeCode--Claude-Sonnet-4.6--Smoke` · `SEMFSClaudeCode--Claude-Sonnet-4.6--Smoke-SEMFS`.
- Plain (non-semfs) variants: drop the `SEMFS_*` vars.
- `DATASET=smoke` = the single case in `tasks_lite/` (we pinned it to **289**; to target another case, swap `tasks_lite/` to contain only that dir, back up the full set first).

### 3.2 Monitor (the right way — they drop otherwise)
```bash
$S -o ServerAliveInterval=15 -o ServerAliveCountMax=10 'bash -lc "
  base=/srv/semfs-benchmark/Workspace-Bench/evaluation
  for i in \$(seq 1 90); do
    aj=\$(ls \$base/output/<LABEL>/289/agent.json 2>/dev/null | head -1)
    if [ -n \"\$aj\" ]; then echo DONE; break; fi
    pgrep -f run_workspace_bench.sh >/dev/null || { echo GONE; tail -n 20 /tmp/<run>.log; break; }
    echo \"tick \$i \$(date -u +%H:%M:%SZ)\"; sleep 22
  done
"'
```
- **Always set `ServerAliveInterval`** — an idle SSH monitor gets reset (we lost 2 monitors to this). The run itself is detached (`setsid`), so a dropped monitor never kills it.
- **Never put `( )` in the remote echo strings** — `bash -lc "echo === foo (bar) ==="` is a syntax error.
- Prepare/mount takes ~6 min before the agent starts; Claude runs add several more.

---

## 4. Verification scripts (copy-paste)

Put these on the host once; run after a test completes. `D=` the run's output dir.

### 4.1 Status + full token breakdown (T5)
```bash
$S 'bash -lc "python3 - <<PY
import json,os
D=\"<D>\"
aj=json.load(open(os.path.join(D,\"agent.json\")))
print(\"status/metrics:\", aj.get(\"status\"), aj.get(\"metrics\"))
rep=json.load(open(os.path.join(D,\"raw\",\"claudecode_report.json\")))   # claude only
u={\"input_tokens\":0,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":0}
for line in rep[\"tasks\"][0].get(\"stdout\",\"\").splitlines():
    try: ev=json.loads(line.strip())
    except: continue
    if isinstance(ev,dict) and ev.get(\"type\")==\"result\":
        usg=ev.get(\"usage\") or {}
        for k in u:
            if isinstance(usg.get(k),int): u[k]+=usg[k]
tot=u[\"input_tokens\"]+u[\"cache_creation_input_tokens\"]+u[\"cache_read_input_tokens\"]+u[\"output_tokens\"]
print(\"usage:\", u, \"TRUE total:\", tot)   # PASS: cache_read/write non-zero; total = sum of 4
PY"'
```

### 4.2 Did Claude READ the hint? (T4 — the canary)
```bash
$S 'bash -lc "grep -roF \"[SEMFS-ACK]\" <D> | wc -l"'     # >0 ⇒ the model received & read the hint
```

### 4.3 Did the agent USE semfs? (T2/T4/T7)
```bash
$S 'bash -lc "
  echo semfs-grep-in-transcript: \$(grep -roF \"semfs grep\" <D> 2>/dev/null | wc -l)
  echo R2-rehydrations: \$(grep -c \"rehydrated raw file\" ~/.cache/semfs/logs/workspace-bench-chanpin.log 2>/dev/null)
  echo shim-routes: \$(grep -c ROUTING /tmp/semfs-shim.log 2>/dev/null)
"'
```
> **Supermemory `/v4/*` API counts come from the DASHBOARD**, not the log — the daemon logs only `rehydrated raw file` (R2 GETs) at INFO, not search/profile calls. R2 GETs ≠ billable API calls.

### 4.4 Tool-call trace (codex vs claude)
```bash
# CLAUDE: tool_use events live in raw/claudecode_report.json → tasks[0].stdout (escaped JSONL); tool name = part.tool
# CODEX:  command_execution items in raw/codex_stdout.jsonl ; field = item.command
$S 'bash -lc "python3 - <<PY
import json
f=\"<D>/raw/codex_stdout.jsonl\"
seen=set()
for line in open(f,errors=\"ignore\"):
    try: it=(json.loads(line).get(\"item\") or {})
    except: continue
    if it.get(\"type\")==\"command_execution\" and it.get(\"id\") not in seen:
        seen.add(it[\"id\"]); print(it.get(\"command\",\"\")[:140])
PY"'
```

### 4.5 Shim interception (T6)
```bash
$S 'bash -lc "
  echo rg-shim-calls: \$(grep -c \"rg argv:\" /tmp/semfs-shim.log 2>/dev/null)   # >0 ⇒ USE_BUILTIN_RIPGREP=0 works
  grep \"rg argv:\" /tmp/semfs-shim.log | grep -v \"\\-\\-files\" | head            # content searches (not startup --files scans)
"'
```

---

## 5. The delivery test (T4) — the canary methodology in full

This is the procedure that finally proved Claude reads the hint. Use it whenever you change how the hint is delivered.

1. The hint (in `CLAUDE.md` / prompt / `systemPrompt`) **must end with**: *"your VERY FIRST line of output must be exactly `[SEMFS-ACK]` followed by the query you will run."*
2. Put the canary in **only one channel** at a time (e.g. only in the project `CLAUDE.md`, not also the prompt) — so an `[SEMFS-ACK]` unambiguously proves *that* channel reached the model.
3. Run; then check the three signals:

| `[SEMFS-ACK]` | `semfs grep` | Conclusion |
|:---:|:---:|---|
| ≥1 | >0 | ✅ **read & complied** — channel works end-to-end |
| ≥1 | 0 | read but ignored → compliance problem (need forcing/injection) |
| **0** | 0 | **never delivered** — the channel is broken (re-check config) |

**Known-good Claude delivery config** (`ClaudeCode.js`, semfs runs only):
```js
fs.writeFileSync(path.join(cwd, 'CLAUDE.md'), hint);   // PROJECT CLAUDE.md
options: { settingSources: ['project'],                // SDK loads CLAUDE.md ONLY with this
           systemPrompt: { type:'preset', preset:'claude_code' } }
```
**Do NOT** use `appendSystemPrompt` (not a real SDK option — silently dropped). Codex's parity path is `~/.codex/AGENTS.md` (the Codex CLI auto-loads it).

---

## 6. Cleanup & isolation (T8)

| Goal | How |
|---|---|
| Don't let the grader skip a case | `rm -rf output/<LABEL>/289` before launch (stale-resume) |
| Cold cache (fair comparison) | `SEMFS_FRESH=1` (wipes `~/.cache/semfs/<org>/<tag>.db` + the target's output dir) — slower (re-hydrates) |
| Clean shim log per run | `rm -f /tmp/semfs-shim.log` |
| Avoid mount timeout on big seed | `SEMFS_MOUNT_TIMEOUT_SEC=900` **and** drop `--clean` (it forces a full re-hydration) |
| Read-only (no seed contamination) | `SEMFS_NO_PUSH=1` (agent's writes are discarded at unmount) |

---

## 7. Failure modes (seen this session) & fixes

| Symptom | Cause | Fix |
|---|---|---|
| monitor exits 255 mid-run | idle SSH reset | `-o ServerAliveInterval=15`; the detached run is unaffected — relaunch the monitor |
| remote `bash -lc` "syntax error near `('" | parens in `echo === … ===` | remove parens |
| Claude never runs `semfs grep`, `[SEMFS-ACK]=0` | hint not delivered (`appendSystemPrompt` phantom / `settingSources:[]` / `HOME=workdir`) | project `CLAUDE.md` + `settingSources:['project']` + preset |
| Claude token total absurdly low (e.g. 5,010) | parser dropped cache fields | sum all four usage fields (§4.1) |
| "0 supermemory calls" but it clearly used semfs | log-grep can't see `/v4/*`; warm cache → 0 live calls | use the **dashboard**; check R2 rehydrations for "was it used" |
| `profile.md` is empty (158 bytes) | it's a *user-profile* feature, blank for document workspaces | expected; not an orientation source today |
| semfs-claude run "passes" instantly, identical numbers | stale resume (output dir exists) | `rm -rf output/<LABEL>/289` |
| `semfs mount` → "daemon exited before ready", 404 | mounting an **unseeded** container | push 1 init doc to create it, then seed |
| mount fails / times out | big seed + `--clean` > default timeout | `SEMFS_MOUNT_TIMEOUT_SEC=900`, drop `--clean` |
| post-run workdir = `Transport endpoint is not connected` | orphaned FUSE mount (teardown race) | adapter's `_force_clear_mount` retry; manual `fusermount3 -u <dir>` |
| shim recurses / hangs | shim's marker-parse used `grep` (re-entered shim on PATH) | parse the `.semfs` marker with pure bash (no `grep`/`cut`) |
| case returns nothing under semfs | case's persona ≠ seeded container | run a case whose workspace is seeded (chanpin/kaifa) |

---

## 8. A full T4 dry-run (end to end, copy-paste)

```bash
S="ssh -i ~/.ssh/semfs-benchmark -o ConnectTimeout=20 -o ServerAliveInterval=15 ubuntu@13.201.35.159"
LBL=SEMFSClaudeCode--Claude-Sonnet-4.6--Smoke-SEMFS
# 1. launch (clean first)
$S 'bash -lc "cd /srv/semfs-benchmark; rm -rf Workspace-Bench/evaluation/output/'$LBL'/289; rm -f /tmp/t4.log /tmp/semfs-shim.log;
  setsid env DATASET=smoke RUN_STAMP=t4 SEMFS_CONTAINER_TAG=workspace-bench-chanpin SEMFS_NO_PUSH=1 SEMFS_MOUNT_TIMEOUT_SEC=900 USE_CLAUDE_LONG_RUNNING_TOKEN=1 \
    /srv/semfs-benchmark/semantic-filesystem/benchmarks/aws/run_workspace_bench.sh semfs-claudecode > /tmp/t4.log 2>&1 </dev/null & echo launched"'
# 2. (run the §3.2 monitor until DONE)
# 3. verify the three signals
$S 'bash -lc "D=/srv/semfs-benchmark/Workspace-Bench/evaluation/output/'$LBL'/289;
  echo status: \$(grep -oE \"\\\"status\\\":\\s*\\\"[a-z]+\\\"\" \$D/agent.json | head -1);
  echo READ_ack: \$(grep -roF \"[SEMFS-ACK]\" \$D | wc -l);
  echo USED_grep: \$(grep -roF \"semfs grep\" \$D | wc -l);
  echo R2_used: \$(grep -c \"rehydrated raw file\" ~/.cache/semfs/logs/workspace-bench-chanpin.log)"'
# PASS: status passed · READ_ack ≥1 · USED_grep >0   → then confirm search_v4 on the dashboard.
```

---

### Related docs
`SEMFS_GRAPHIFY_DESIGN.md` (architecture, algorithms, full experiment log) · `SEMFS_BENCHMARK_RUNBOOK.md` (seeding, dataset, machine, deeper failure modes) · the `semfs_*.html` explainers · `rcas/` (root-cause histories).

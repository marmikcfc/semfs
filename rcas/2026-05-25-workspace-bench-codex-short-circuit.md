# RCA: Workspace-Bench Codex short-circuits to a path list without doing work

Date: 2026-05-25

## Summary

`Codex` and `SEMFSCodex` were failing Workspace-Bench smoke task `100` on the EC2 benchmark host even after host cleanup, valid `semfs` auth, and working FUSE mounts. The direct cause of the task failure was that Codex returned a final Python path list immediately, without reading source files or writing the deliverable. The benchmark then failed with `Agent returned empty path list` because no output file existed.

The primary root cause is the benchmark prompt wrapper in `agent_runner.py`. It injects two problematic constraints:

1. It tells the agent to write outputs into `filesys/<workdir>/model_output` even though the current working directory is already `.../filesys/<workdir>`.
2. It ends with a strict instruction to output only a Python list of file paths.

In manual reproduction, removing the strict final-list block caused Codex to use tools and complete the task. Keeping the exact benchmark prompt caused Codex to short-circuit and return only a list-shaped answer.

## User-visible symptoms

- Plain `Codex` failed smoke task `100`
- `SEMFSCodex` failed the same task after mount succeeded
- `executionTrace` showed one assistant turn and zero tool calls
- `checks[0].detail` was `Agent returned empty path list`
- In some earlier contaminated runs, Codex also logged:
  - `ERROR codex_core::agents_md: error trying to find AGENTS.md docs: Socket not connected (os error 107)`

## Impact

- Benchmark accuracy numbers for current Codex smoke runs are not meaningful as a comparison of plain filesystem vs SEMFS.
- Latency and token numbers remain useful as infrastructure smoke data, but not as a quality comparison.
- The stale-FUSE issue was real, but it was not the final blocker once the host was cleaned.

## Execution path

1. Workspace-Bench wraps the task prompt in `evaluation/src/agent_runner.py`.
2. `_wrap_prompt(...)` prepends the working-directory restriction and this output-path override:
   - `请你无视任务要求中的输出文件保存路径要求，将所有输出文件放置在目录：{os.path.join(work_dir, task_target_output_dir)}下`
3. For task `100`, that becomes:
   - `filesys/houqin_workdir_Codex_GPT-5.4/model_output`
4. The Codex runner launches `codex exec` in the workdir with a localhost chat adapter.
5. Codex returns a single final assistant message containing only a Python list string.
6. `_collect_output_paths(...)` in `agent_runner.py` tries to resolve listed paths and discover outputs, but no file exists, so `output_paths=[]`.
7. The run is marked failed with `Agent returned empty path list`.

Relevant benchmark code:

- `/srv/semfs-benchmark/Workspace-Bench/evaluation/src/agent_runner.py:223`
- `/srv/semfs-benchmark/Workspace-Bench/evaluation/src/agent_runner.py:318`

## Evidence

### 1. Exact benchmark prompt causes immediate short-circuit

From `agent.json` for the clean plain Codex run:

- Workdir: `/srv/semfs-benchmark/Workspace-Bench/evaluation/filesys/houqin_workdir_Codex_GPT-5.4`
- Assistant response:
  - `['filesys/houqin_workdir_Codex_GPT-5.4/model_output/onsite_hosting_execution_manual.doc']`
- Tool calls: none
- Output files: none

From `agent.json` for the clean SEMFS Codex run:

- Assistant response:
  - `['model_output/onsite_hosting_execution_manual.doc']`
- Tool calls: none
- Output files: none

This shows the failure is not specific to SEMFS mount success. Both variants short-circuit.

### 2. Manual reproduction outside Workspace-Bench changed behavior

Direct manual Codex run in the same workdir with the exact benchmark prompt:

- Returned a list-shaped final answer
- Did not perform workspace reads/writes

Direct manual Codex run in the same workdir with the final strict list block removed:

- Used command tools (`pwd`, `rg --files`, `sed`, Python zip/docx extraction)
- Identified source files
- Wrote the output file
- Verified the output file
- Returned the final path list

This is the strongest evidence that prompt shape is the primary behavioral trigger.

### 3. Host filesystem was writable

Manual shell writes in the benchmark workdir succeeded:

- `mkdir -p model_output`
- `echo test > model_output/_manual_write_probe.txt`

So the failure was not caused by a read-only host workdir.

### 4. Stale FUSE mounts were real but secondary

Earlier host state showed:

- `Transport endpoint is not connected`
- `Socket not connected (os error 107)`
- stale FUSE mount still present while `semfs list` showed no active mounts

That contamination was cleaned up and guarded against in the runner. After cleanup:

- the `agents_md` socket error disappeared
- plain Codex still failed in the same short-circuit pattern

So stale mounts were a separate infrastructure bug, not the final root cause for the Codex task failure.

## Root cause

Primary root cause:

- The benchmark prompt wrapper over-constrains Codex into output-format compliance instead of task execution.

Contributing causes:

1. Wrong output directory hint
   - The runner tells the agent to write into `filesys/<workdir>/model_output` instead of a directory relative to the current workdir such as `model_output`.
   - This encourages the model to emit a nested path string instead of treating `model_output/...` as a normal relative deliverable path.

2. Overly strong final-output instruction
   - The prompt ends with “only output a Python list”.
   - For Codex on this provider/model path, that instruction is strong enough to cause early finalization without tool use.

3. Benchmark smoke pass on Railway was not preserved with raw artifacts
   - We cannot do a strict passing-vs-failing artifact diff against Railway because the raw Railway run artifacts were not retained in the current workspace.
   - So we can explain the current EC2 failure path rigorously, but not fully explain the earlier Railway pass delta.

## Non-causes / disproven causes

Not the cause:

- missing `SUPERMEMORY_API_KEY`
- invalid `semfs` credentials
- missing `user_allow_other`
- stale FUSE mounts after the host cleanup
- unwritable benchmark workdir

These were either fixed or disproven during manual reproduction.

## Fixes already made

1. Cleaned stale FUSE mounts before every run
2. Fixed SEMFS adapter mount false-negative handling
3. Added guaranteed SEMFS unmount in `finally`
4. Added richer SEMFS failure diagnostics
5. Added workspace telemetry and narrative artifacts

## Recommended corrective action

1. Fix the benchmark prompt wrapper
   - Change the output override from:
     - `os.path.join(work_dir, task_target_output_dir)`
   - to:
     - `task_target_output_dir`
   - The instruction should tell the agent to write to `model_output/...` relative to the current workdir.

2. Relax the final output formatting instruction
   - Keep the requirement to provide final paths.
   - Remove or soften the “only output a Python list” hard constraint for Codex.

3. Re-run the four-way smoke comparison after the prompt fix
   - `codex`
   - `semfs-codex`
   - `claudecode`
   - `semfs-claudecode`

4. Preserve raw artifacts for every run
   - especially `codex_stdout.jsonl`, `codex_invocation.json`, `last_message.txt`, and `agent.json`

## Proposed validation

After fixing the wrapper:

1. Manual Codex reproduction with the exact benchmark prompt should show tool calls and a created output file.
2. Plain Workspace-Bench `Codex` smoke should no longer fail with `Agent returned empty path list`.
3. `SEMFSCodex` should produce the same task result shape, allowing an actual plain-vs-SEMFS comparison.


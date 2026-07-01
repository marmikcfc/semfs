#!/usr/bin/env python3
"""Paired token-ceiling gate for the semfs grep-delivery optimization.

Objective: an experiment must use NO MORE tokens than the PLAIN baseline (the other half of
"beat plain on both axes" — accuracy is the maximized score; tokens are this hard constraint).

Reads the harness sidecar (.evo_bench_metrics.json, a fixed worktree-relative path because
evo gates do NOT receive EVO_* artifact env) and exits non-zero if mean_tokens exceeds the
ceiling. Post-phase gate (runs after the benchmark). Exit 0 = pass, 1 = regression.

  python3 {worktree}/benchmarks/e2b/evo_token_gate.py --worktree {worktree} --ceiling <PLAIN_MEAN_TOKENS>
"""
import argparse, json, pathlib, sys


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--worktree", required=True)
    ap.add_argument("--ceiling", type=float, required=True,
                    help="plain baseline mean tokens; experiment must be <= this")
    args = ap.parse_args()
    sidecar = pathlib.Path(args.worktree) / ".evo_bench_metrics.json"
    if not sidecar.exists():
        print(f"GATE FAIL: no metrics sidecar at {sidecar} (benchmark did not run?)", file=sys.stderr)
        sys.exit(1)
    m = json.loads(sidecar.read_text())
    mt = m.get("mean_tokens")
    if mt is None:
        print("GATE FAIL: mean_tokens missing from sidecar", file=sys.stderr)
        sys.exit(1)
    if mt > args.ceiling:
        print(f"GATE FAIL: mean_tokens {mt:.0f} > plain ceiling {args.ceiling:.0f} "
              f"(must be cheaper than plain)", file=sys.stderr)
        sys.exit(1)
    print(f"GATE PASS: mean_tokens {mt:.0f} <= plain ceiling {args.ceiling:.0f}")
    sys.exit(0)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
import json, statistics
from collections import defaultdict

CELLS = json.load(open("/Users/marmikpandya/semantic-filesystem/tickets/workspace-bench-5arm-matrix/_token_attrib/cells.json"))

def mean(xs):
    xs = [x for x in xs if x is not None]
    return statistics.mean(xs) if xs else 0
def med(xs):
    xs = [x for x in xs if x is not None]
    return statistics.median(xs) if xs else 0

print("="*120)
print("PER-ARM AGGREGATE (n cells, prompt_tokens, completion, turns, tok/turn, semfs_grep calls, deliverables, accuracy, timeouts)")
print("="*120)
hdr = f"{'arm':<22}{'n':>3}{'med_prompt':>11}{'mean_prompt':>12}{'mean_compl':>11}{'med_turns':>10}{'med_tok/turn':>13}{'mean_grep':>10}{'mean_deliv':>11}{'mean_acc':>9}{'timeouts':>9}"
print(hdr)
byarm = defaultdict(list)
for c in CELLS: byarm[c["arm"]].append(c)
arm_rows = {}
for arm in sorted(byarm):
    cs = byarm[arm]
    n = len(cs)
    prompts = [c["prompt_tokens"] for c in cs]
    compl = [c["completion_tokens"] for c in cs]
    turns = [c["turns"] for c in cs]
    tokperturn = [c["prompt_tokens"]/c["turns"] if c["turns"] else 0 for c in cs]
    greps = [c["n_semfs_grep"] for c in cs]
    delivs = [c["n_deliverables"] for c in cs]
    accs = [c["accuracy"] for c in cs if c["accuracy"] is not None]
    timeouts = sum(1 for c in cs if c["status"] == "timeout")
    arm_rows[arm] = dict(n=n, med_prompt=med(prompts), mean_prompt=mean(prompts),
                         mean_compl=mean(compl), med_turns=med(turns), med_tokperturn=med(tokperturn),
                         mean_grep=mean(greps), mean_deliv=mean(delivs), mean_acc=mean(accs), timeouts=timeouts)
    print(f"{arm:<22}{n:>3}{med(prompts):>11.0f}{mean(prompts):>12.0f}{mean(compl):>11.0f}{med(turns):>10.0f}{med(tokperturn):>13.0f}{mean(greps):>10.1f}{mean(delivs):>11.1f}{mean(accs):>9.2f}{timeouts:>9}")

print()
print("="*120)
print("PER-CASE AGGREGATE (across all arms): difficulty signal")
print("="*120)
print(f"{'case':<6}{'n':>3}{'med_prompt':>11}{'mean_prompt':>12}{'med_turns':>10}{'mean_acc':>9}{'timeouts':>9}{'mean_deliv':>11}")
bycase = defaultdict(list)
for c in CELLS: bycase[c["case"]].append(c)
for case in sorted(bycase, key=int):
    cs = bycase[case]
    prompts = [c["prompt_tokens"] for c in cs]
    turns = [c["turns"] for c in cs]
    accs = [c["accuracy"] for c in cs if c["accuracy"] is not None]
    timeouts = sum(1 for c in cs if c["status"] == "timeout")
    delivs = [c["n_deliverables"] for c in cs]
    print(f"{case:<6}{len(cs):>3}{med(prompts):>11.0f}{mean(prompts):>12.0f}{med(turns):>10.0f}{mean(accs):>9.2f}{timeouts:>9}{mean(delivs):>11.1f}")

print()
print("="*120)
print("RE-PREFILL MODEL CHECK: does prompt_tokens ~= turns * mean_per_turn? and how well does weighted-output-char explain it?")
print("="*120)
# correlate prompt_tokens with turns and with reprefill_weighted_chars
import math
def pearson(xs, ys):
    pairs = [(x,y) for x,y in zip(xs,ys) if x is not None and y is not None]
    if len(pairs) < 3: return float('nan')
    xs2=[p[0] for p in pairs]; ys2=[p[1] for p in pairs]
    mx=mean(xs2); my=mean(ys2)
    num=sum((x-mx)*(y-my) for x,y in pairs)
    den=math.sqrt(sum((x-mx)**2 for x in xs2)*sum((y-my)**2 for y in ys2))
    return num/den if den else float('nan')
P=[c["prompt_tokens"] for c in CELLS]
T=[c["turns"] for c in CELLS]
W=[c["reprefill_weighted_chars"] for c in CELLS]
G=[c["grep_output_chars"] for c in CELLS]
TOT=[c["total_tool_output_chars"] for c in CELLS]
print(f"corr(prompt_tokens, turns)                 = {pearson(P,T):.3f}")
print(f"corr(prompt_tokens, reprefill_weighted_chars)= {pearson(P,W):.3f}")
print(f"corr(prompt_tokens, total_tool_output_chars) = {pearson(P,TOT):.3f}")
print(f"corr(prompt_tokens, grep_output_chars)       = {pearson(P,G):.3f}")
# turns*per-turn identity check on a few extremes
print()
print("Top 12 cells by prompt_tokens:")
print(f"{'dir':<48}{'status':>8}{'prompt':>8}{'turns':>6}{'tok/turn':>9}{'grep#':>6}{'gchars':>8}{'maxout':>8}{'acc':>6}")
for c in sorted(CELLS, key=lambda c:-c["prompt_tokens"])[:12]:
    tpt = c["prompt_tokens"]/c["turns"] if c["turns"] else 0
    acc = c["accuracy"]
    print(f"{c['dir']:<48}{str(c['status']):>8}{c['prompt_tokens']:>8}{c['turns']:>6}{tpt:>9.0f}{c['n_semfs_grep']:>6}{c['grep_output_chars']:>8}{c['max_tool_output_chars']:>8}{('%.2f'%acc) if acc is not None else 'NA':>6}")
print()
print("Bottom 8 cells by prompt_tokens (low-token contrast):")
print(f"{'dir':<48}{'status':>8}{'prompt':>8}{'turns':>6}{'tok/turn':>9}{'grep#':>6}{'gchars':>8}{'acc':>6}")
for c in sorted(CELLS, key=lambda c:c["prompt_tokens"])[:8]:
    tpt = c["prompt_tokens"]/c["turns"] if c["turns"] else 0
    acc = c["accuracy"]
    print(f"{c['dir']:<48}{str(c['status']):>8}{c['prompt_tokens']:>8}{c['turns']:>6}{tpt:>9.0f}{c['n_semfs_grep']:>6}{c['grep_output_chars']:>8}{('%.2f'%acc) if acc is not None else 'NA':>6}")

print()
print("="*120)
print("ACCURACY-DROP BUCKETS (cells with accuracy < 0.5 or None)")
print("="*120)
buckets = defaultdict(list)
for c in CELLS:
    acc = c["accuracy"]
    if acc is None or acc < 0.5:
        if c["n_deliverables"] == 0:
            b = "A_empty_deliverable"
        elif c["status"] == "timeout":
            b = "B_timeout_overexplore"
        elif acc is not None and acc < 0.5:
            b = "C_wrong_or_hollow_content"
        else:
            b = "D_other"
        buckets[b].append(c)
for b in sorted(buckets):
    print(f"{b}: {len(buckets[b])} cells")
    for c in sorted(buckets[b], key=lambda c: c['arm']):
        acc = c["accuracy"]
        print(f"    {c['dir']:<46} arm={c['arm']:<20} status={c['status']:<8} turns={c['turns']:<3} deliv={c['n_deliverables']} acc={('%.2f'%acc) if acc is not None else 'NA'}")

print()
print("="*120)
print("STATUS / TIMEOUT distribution per arm")
print("="*120)
for arm in sorted(byarm):
    cs = byarm[arm]
    ok = sum(1 for c in cs if c["status"]=="ok")
    to = sum(1 for c in cs if c["status"]=="timeout")
    other = len(cs)-ok-to
    print(f"{arm:<22} ok={ok} timeout={to} other={other}")

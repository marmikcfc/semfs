# Strong opinions, explicitly falsifiable (2026-06-11)

> Formed per the `forming-opinions` discipline: gut named as prior → identity audit →
> steel-man → credence → falsifier. Inputs: E1–E5 artifacts ([`RESULTS.md`](RESULTS.md)),
> the decomposition ([`TOKEN_ECONOMY.md`](TOKEN_ECONOMY.md)), external evidence
> ([`RESEARCH_NOTES_EXTERNAL.md`](RESEARCH_NOTES_EXTERNAL.md)). Each falsifier maps to an
> experiment in [`EXPERIMENTS_NEXT.md`](EXPERIMENTS_NEXT.md).

**Standing identity-audit (applies to all):** semfs is our product. The motivated
conclusion is "semfs can win"; the threat is admitting the product has no edge on this
benchmark. Every opinion below was checked against the strongest contrary evidence we have
— plain's across-the-board WB win and the PwC finding that grep is near-optimal on small,
well-named corpora. O8 exists *because* of this audit.

---

## O1 — Delivery form is the #1 token lever (not retrieval quality, not KG, not output compression)

**Gut (prior):** "Capping/structuring what a search call puts into context matters more
than anything else we can change." **Type:** factual.

**Opinion** — **~85%**. The per-search-call payload (size, content choice, position) is the
single highest-impact token lever available to semfs on WB.
**Load-bearing:** grep-cap fix alone moved 289 from 111–145K → 76.8K (first-ever sub-plain
run); a single uncapped grep returned 142K chars; 95_cloud's clip discovery shows payload
control is currently delegated to a blind harness layer; field consensus #3 (Firetiger/
Anthropic: control what enters context).
**Steel-man against:** turn-count variance (5 vs 16 calls) swamps payload effects — run 2
lost the win despite the cap; maybe behavior (hint) is the real #1 and delivery is #2.
**I'd change my mind if:** E8 (n=3, hint fixed) shows capped-delivery arms within ±10% of
uncapped on tokens — that would crown the hint (O2) as #1 and demote delivery.
**Pre-commit:** if falsified, stop tuning render caps and move all effort to hint/behavior.

## O2 — The ~2KB agent hint outweighs the entire Rust retrieval pipeline

**Gut (prior):** "The hint is the product." **Type:** identity-fused (the pipeline is
months of engineering; admitting a text file dominates it stings).

**Opinion** — **~80%**. Rewriting the injected hint (kill "read kg/ FIRST", kill
"trust-the-excerpt, don't open files", replace with "grep 2–4 key terms → read the single
top hit for exact values → don't crawl") cuts ≥25% of tokens on 289/15-class cases at
accuracy ≥ parity, and reduces call-count variance.
**Load-bearing:** the KG-hint cascade is 58K+/run of directly-attributed waste (35.7K KG
read + 22.4K os.walk); the kg-scoped grep returning 138 chars is the verified
confidence-collapse trigger; PwC: same corpus, same retriever, 38pp swing from
harness/prompting alone.
**Steel-man against:** codex may ignore hints entirely (memory: Claude Code ignored the
semfs grep hint — maybe codex's obedience is overstated and the KG read is its own
curiosity); also the hint is baked read-only into the seed, so the fix may be impractical.
**I'd change my mind if:** E7 hint-ablation (n=3) shows <10% token delta between hint
variants on the same seed/binary.
**Pre-commit:** if falsified, the lever moves to harness-level affordances (file naming,
workspace map) rather than instructions — i.e., E13 jumps the queue.

## O3 — The full "scout stack" beats plain on tokens at accuracy parity on ≥3/5 WB cases

**Gut (prior):** "We can finally win both axes locally." **Type:** identity-fused
(this is the product's headline claim; flagged accordingly — hence the pre-registered
kill condition below).

**Opinion** — **~60%** (deliberately not higher: n=2 evidence, one win, one miss).
Stack = clean infra + `SEARCH_ONLY=off` + grep cap 6KB/rlim 3 + rewritten hint + KG
suppressed. Mechanics: 1 search replaces 4–8 find-probes (the ~50K plain wastes re-paying
discovery), capped payload keeps oₛ small, hint stops the crawl.
**Load-bearing:** each component independently verified (H1, H2, grep-cap win, RC3);
what's untested is only the *conjunction*.
**Steel-man against:** plain's probes are cheap *and reliable*; semfs adds S (hint bytes)
to every turn; codex variance may eat the margin; PwC says grep-on-this-corpus is
near-optimal so the discovery savings may be smaller than Chain B estimates.
**I'd change my mind if:** E8 (5 cases × n=3) shows <2/5 cases with mean tokens < plain at
accuracy ≥ plain−1. **That is the kill condition for "local semfs wins WB on tokens" —
if it fails, stop optimizing for WB-chanpin and execute O8.**

## O4 — Summaries are an accuracy lever only, and WB-chanpin cannot measure them

**Gut (prior):** "Summaries are the magic that made cloud win 95." **Type:** factual.

**Opinion** — **~75%**. Per-doc/sheet LLM summaries lift accuracy only when the agent must
*semantically search* for an unnamed answer source among many similar files; zero WB-chanpin
case has that shape (44 names files, 289=403 stubs, 95=txt, 15=ceiling, 175=csv). They are
token-neutral at best (dual-store: FIND on summary, ANSWER from raw table).
**Load-bearing:** summary RCA structural analysis; the one valid A/B (44 dual-store
2/16 vs raw 4/16, token-neutral, both read identical tables); cloud's 95 win came with
*summary-quality excerpts inline* — confounded with coverage (0/12 on 175).
**Steel-man against:** 95 IS evidence summaries help synthesis-style cases; maybe 175-class
synthesis cases benefit too and we just never ran local+summaries there at full coverage.
**I'd change my mind if:** E11's discovery-stressed cases show summary-arm accuracy ≤ raw
arm; or conversely if a local+summaries run wins 95 ≥11/12 AND 175 >0 (then summaries are a
today-lever on existing cases, not just new ones).

## O5 — The cache-blind token metric distorts the production story; report cache-adjusted cost alongside

**Gut (prior):** "We're optimizing a number production doesn't pay." **Type:** factual.

**Opinion** — **~70%** that cache-adjusted accounting materially changes at least one arm
ranking (most likely: high-turn arms look much less bad, since repeated context becomes
~10× cheaper; arms that differ in *unique* bytes — KG reads, blobs — keep their penalty).
**Load-bearing:** Manus production numbers ($0.30 vs $3/MTok); `cached_input=0` on every
WB run is an endpoint artifact, not a product property; under caching the cost function
collapses from `Σ(T−s)·oₛ` toward `Σoₛ + T·ε`.
**Steel-man against:** worst-case (no-cache) cost is a legitimate, conservative metric;
ripbench may never serve caching, so the raw number is what this harness actually bills.
**I'd change my mind if:** E10 re-scoring shows all arm *orderings* unchanged under
cache-adjusted pricing — then the metric choice is moot and we drop the complaint.

## O6 — Caveman (output compression) is a ≤8% lever on WB; do not prioritize it for the benchmark

**Gut (prior):** "Cute, not decisive." **Type:** factual.

**Opinion** — **~70%**. Measured G = 1.0–3.0K output vs 78–138K input (≈2–4% of tokens;
≤8–15% of *cost* at 4–5× output pricing). codex is already terse (external corroboration).
Ship it as product polish; don't expect WB movement.
**I'd change my mind if:** E12 shows >10% total-token reduction on any WB case (would mean
narration is bigger than traces indicate, e.g. hidden reasoning tokens count as output).

## O7 — KG and /by-topic/ as agent-facing surfaces are net-negative for codex-class agents; kill or gate behind a ≤4KB digest

**Gut (prior):** "The KG is a tax." **Type:** identity-fused (KG was a major feature
investment — T2/T3 work; flagged).

**Opinion** — **~85%**. Every measurement agrees: gfs_on = worst tokens in the matrix
(471K mean, 666K worst); KG read = 35.7K at turn 1 (worst position); /by-topic/ invites
crawling (12–24 calls); H5 confirmed; zero accuracy payoff in any cell.
**Steel-man against:** the KG may pay off for *orientation* on genuinely unfamiliar large
corpora or for multi-session agents with memory — WB's single-shot small-corpus setting is
its worst case; also gfs_on *tied* plain accuracy on 289 (6/15).
**I'd change my mind if:** a ≤4KB digest arm (E7c) beats nokg on tokens or accuracy — that
would rehabilitate KG-as-digest while still killing KG-as-61KB-file.

## O8 — WB-chanpin is the wrong arena to prove semfs; the winnable arenas are format-trap, cross-lingual, and discovery-stressed corpora

**Gut (prior):** "Stop fighting where grep is king." **Type:** identity-fused — this is
the uncomfortable one (it says: even if E8 succeeds, a ±5% token win on this benchmark is
a weak product story).

**Opinion** — **~70%**. On a 1452-file corpus with filename-semantic ground truth in clean
txt/csv, plain `find|grep` is near-optimal (PwC paper agrees; plain's 46%@89K agrees).
semfs's *demonstrated* edges are elsewhere: format trap (.extracted.md: −80% tokens,
PROVEN), cross-lingual ranking (#417→#1, PROVEN), and unnamed-answer discovery (untested —
E11). The product narrative should lead with acquisition + cross-lingual, not raw token
counts on grep-friendly ground.
**Steel-man against:** "the benchmark is wrong" is exactly what motivated reasoning would
say after losing — and customers' workspaces may look more like WB than like our ideal
corpus. Also Codex DID get −75% from semfs in the original affordance test (memory), so
semfs CAN win agent benchmarks when delivery works.
**I'd change my mind if:** E8 produces a *consistent* (n=3) both-axes win on WB — then
semfs wins even on hostile ground and this opinion is moot; OR if E11's discovery-stressed
cases show no semfs advantage either — then the problem is deeper than arena choice.
**Pre-commit:** if E8 fails AND E11 succeeds → re-aim the benchmark roadmap at the adapter
ticket's xAFS/terminal-bench direction with discovery-stressed + cross-lingual case sets.

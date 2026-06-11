# How the field optimizes these components (external research, 2026-06-11)

> Gathered via composio (Reddit, X/Twitter, Exa) + web. Each finding is mapped to the
> component tree in [`TOKEN_ECONOMY.md`](TOKEN_ECONOMY.md) §2. Read before designing any
> new experiment — several of our "novel" ideas already have published numbers.

## The one-table summary

| Source | Finding | Maps to component | Status for us |
|---|---|---|---|
| **PwC, "Is Grep All You Need?"** [arXiv 2605.15184](https://arxiv.org/abs/2605.15184) (May 2026) | Harness & delivery, not the retriever, dominate agentic-search accuracy. **Codex/GPT-5.4: 93.1% with INLINE grep results vs 55.2% file-based** — the biggest harness effect measured. Grep beat vectors inline (76.7–93.1% vs 62.9–83.6%); vectors won 5/10 pairs file-based. | delivery form, hint | Validates GREP_INLINE=on for codex; warns that "read the file yourself" delivery (SEARCH_ONLY=off flows) is a tool-use stress test. Our E1–E5 independently rediscovered their headline. |
| **GrepAI benchmark** ([reddit 908↑](https://www.reddit.com/r/ClaudeAI/comments/1qiv0d3/), [protocol](https://yoanbernabeu.github.io/grepai/blog/benchmark-grepai-vs-grep-claude-code/)) | Local semantic search replacing grep-exploration: **−97% input tokens in the DISCOVERY phase** (51K→1.3K), −27.5% total cost, 0 subagents (vs 5), on a 155K-line repo. | discovery turns | The savings is *discovery-phase only* — exactly our Chain B. Note their corpus is 10× our WB corpus; effect should shrink on small corpora. |
| **Cognition SWE-grep** ([blog](https://cognition.ai/blog/swe-grep)) | Devin/Windsurf telemetry: **agents spend >60% of their first turn on context retrieval.** Fix: an RL-trained *fast retrieval subagent* (8 parallel tool calls/turn, ≤4 turns, 2,800 tok/s on Cerebras) that returns only the needed context — context isolation. | discovery turns, T | The subagent pattern = "discovery happens OUTSIDE the billed context". semfs CLI could play this role without any model: see E14. |
| **Manus, context-engineering lessons** ([blog](https://manus.im/blog/Context-Engineering-for-AI-Agents-Lessons-from-Building-Manus)) | KV-cache is THE production metric: cached input **10× cheaper** ($0.30 vs $3/MTok). Append-only context, stable serialization, mask-don't-remove tools, **file-system-as-memory** (drop content, keep restorable pointers), keep errors in context. | cache accounting (E10), S stability | WB's `cached_input=0` measures the no-cache worst case — production cost ranking can differ. Also: "FS as memory" is literally semfs's thesis, said by someone else's production team. |
| **Firetiger, very-large tool results** ([blog](https://blog.firetiger.com/agent-engineering-patterns-dealing-with-very-large-tool-results/)) | Two patterns: (a) truncate **with notice placed BEFORE the content** + suggest narrower queries; (b) **saved artifacts + jq-filter tool** — full result stored, model slices it. Artifacts: 94% one-shot success, median 31s→6s. | oₛ control | (a) is a 1-line improvement to our `TRUNCATED` marker (put notice first, suggest refinement). (b) = E14 (`semfs slice`). |
| **Anthropic, code execution with MCP** ([blog](https://www.anthropic.com/engineering/code-execution-with-mcp)) | Let the agent filter tool results in an execution environment so only the final small result enters context: **150K→2K tokens (−98.7%)** on tool-heavy flows. | oₛ control | Same mechanism as Firetiger artifacts. The agent already HAS python in WB — the missing piece is semfs exposing sliceable results instead of blobs. |
| **DeepMind embedding-ceiling** ([arXiv 2508.21038](https://arxiv.org/abs/2508.21038)) | Theoretical recall ceiling for single-vector embeddings as corpus grows (e.g. ~500K docs @ 512d). | discovery (embedder) | Don't over-invest in embedder quality for WB-size corpora; the ceiling argument cuts the other way at scale. |
| **r/ClaudeCode "Output tokens are the real cost"** ([post](https://www.reddit.com/r/ClaudeCode/comments/1sz87j3/)) + comments | Output tokens are 4–5× pricier and agents burn them narrating exploration. BUT: top comment notes codex is already terse vs Claude; and "shorter output chains compound with prompt caching." | G (caveman ticket) | Our traces: G = 1–3K vs input 78–138K → on WB, output-side compression is a ≤8% lever. Real but not the bottleneck. |
| **r/hermesagent "95% token cut"** ([post](https://www.reddit.com/r/hermesagent/comments/1ths4dt/)) | Tree-structured lazy bootstrap (index files, load on demand), "hand deterministic work to CPU/scripts, reserve the LLM for reasoning." | S, oₛ | Supports the ≤1KB workspace-map idea (E13) and `semfs slice` (E14). |

## Synthesis — what the field agrees on

1. **Discovery is the token sink** (Cognition >60%, GrepAI −97% discovery-phase). Search
   should *replace* exploration turns, not add a parallel surface to crawl. Matches our
   Chain B exactly.
2. **The harness/delivery layer dominates the retriever** (PwC paper: same data, same
   corpus, 38pp swing from delivery form alone). Matches our F6/RC3: the 2KB hint outweighs
   the Rust pipeline.
3. **Control what enters context; store the rest behind a pointer** (Firetiger artifacts,
   Anthropic code-exec, Manus FS-as-memory). Nobody ships uncapped blobs; best practice is
   capped + notice + a slicing affordance for follow-up.
4. **Cache-awareness changes the economics 10×** (Manus). A benchmark that zeroes the cache
   measures something real (worst case) but not the production bill.
5. **Inline beats file-based for codex specifically** (PwC: 93.1 vs 55.2). For our agent,
   the excerpt should carry the answer when it can; file-reads are the fallback, not the
   primary path. This *tempers* the "path-first" idea: paths-only delivery risks the
   file-based failure mode → test as E9 A/B, don't assume.

## Where we differ from the field (defensible novelty)

- Nobody in the surveyed material owns the **filesystem mount as the delivery surface** —
  the `.extracted.md` sibling (format-trap fix, proven −80% tokens) is a delivery mechanism
  the harness papers don't model. semfs's edge is *acquisition* (binary→readable), not
  *discovery*, on grep-friendly corpora.
- **Cross-lingual retrieval** (case-289 rewrite: rank #417→#1) is absent from all surveyed
  benchmarks — grep cannot do this at all. It is semfs's cleanest possible win condition (E11).

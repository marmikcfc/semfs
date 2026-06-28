# Composio-sourced findings — AI/algo trading on Polymarket & Kalshi
_Gathered 2026-06-14 via Composio (Reddit + Exa/web). Twitter/X unavailable: connected API account returned `402 API credits depleted`._
_Backbone web+academic verification running in parallel via the `deep-research` workflow (adversarial 3-vote harness)._

## Headline / the honest finding
**The only real-capital benchmark of autonomous AI trading shows losses.** *Prediction Arena* (arXiv 2604.07355) ran 6 frontier models trading real money on Kalshi & Polymarket for 57 days (Jan 12–Mar 9, 2026); Kalshi final returns ranged **−16.0% to −30.8%**. Crucially: "performance mainly driven by initial prediction accuracy and ability to monetize correct predictions; **research volume had little correlation with outcomes.**" → naive "more auto-research = more profit" is NOT supported.

Corroborating concentration stat (multiple sources): ~**67% of Polymarket profits accrue to the top 0.1% of accounts** (WSJ analysis, via r/u_raytoei). 0xinsider: top-10 wallets = $96.5M combined P&L ≈ 21% of the $463.5M earned by only ~7,070 profitable traders (≈0.07% of accounts). Edge is real but extremely concentrated and hard to copy.

---
## 1. CENSUS — who is doing this

### A) Open-source GitHub bots (hobbyist/practitioner camp) — overwhelmingly ARBITRAGE
Almost all converge on the SAME idea: cross-venue Polymarket↔Kalshi arb + 15-minute crypto markets.
- **zostaff/poly-arbitrage-bot** — 22★, Python, MIT. Cross-exchange Polymarket↔Kalshi arb; paper+live modes; intra-market bundling; needs `py-clob-client`. https://github.com/zostaff/poly-arbitrage-bot
- **defi-ape/polymarket-kalshi-arbitrage-bot** — 15-min arb; polls Kalshi Trade API + Polymarket CLOB; detects YES/UP spreads + late-resolution gaps. https://github.com/defi-ape/polymarket-kalshi-arbitrage-bot
- **up2itnow0822/multi-clob-arb-scanner** — multi-CLOB arb scanner; "Built after Polymarket acquired Dome." https://github.com/up2itnow0822/multi-clob-arb-scanner
- **CraftyGeezer/Kalshi-Polymarket-Ai-bot** — 564★ but only 1 fork / 564 watchers (suspicious ratio — treat with skepticism; possible inflated/scam). Python+Rust. https://github.com/CraftyGeezer/Kalshi-Polymarket-Ai-bot
- **soldino777/polymarket-arb-bot** — cross-market arb, executes both legs simultaneously. https://github.com/soldino777/polymarket-arb-bot
- **ImMike/polymarket-arbitrage** — watches 10,000+ markets for inefficiencies. https://github.com/ImMike/polymarket-arbitrage
- **zostaff/poly-trading-bot** — 28★, Python, MIT. https://github.com/zostaff/poly-trading-bot
- **456ape/kalshi-polymarket-arb** — cross-platform arb, both legs simultaneously, locks in spread. https://github.com/456ape/kalshi-polymarket-arb

Takeaway: **dozens** of repos, mostly low-star, mostly identical arb logic. The crowd does arbitrage, not LLM alpha. Several are scams (see reality-check below).

### B) Academic / research camp — LLM forecasting → real-capital trading
- **Halawi et al. 2024**, "Approaching Human-Level Forecasting with Language Models" (NeurIPS 2024). LMs forecast near competitive human forecaster level. https://proceedings.neurips.cc/paper_files/paper/2024/file/5a5acfd0876c940d81619c1dc60e7748-Paper-Conference.pdf
- **ForecastBench** (Karger et al., ICLR 2025; arXiv 2409.19839). Dynamic real-time benchmark; questions from prediction markets/platforms; evaluates GPT-3.5/4, Claude vs expert & public forecasts. https://arxiv.org/abs/2409.19839
- **Metaculus AI Forecasting Benchmark** (2025).
- **"Wisdom of the Silicon Crowd"** (arXiv 2402.19379) — LLM ensemble rivals human crowd accuracy. https://arxiv.org/html/2402.19379v5
- **"Evaluating LLMs on Real-World Forecasting Against Expert Forecasters"** (arXiv 2507.04562, Jul 2025).
- **AIA Forecaster: Technical Report** (arXiv 2511.07678) — LLM judgmental forecasting from unstructured data, 3 core elements.
- **"Future Is Unevenly Distributed: Forecasting Ability of LLMs Depends on What We're Asking"** (OpenReview uOGC1unggG).
- **"Agentic Forecasting using Sequential Bayesian Updating of Linguistic Beliefs"** (arXiv 2604.18576) — agentic + Bayesian updating (directly on-thesis).
- **Prediction Arena** (arXiv 2604.07355) — 6 frontier models trade real $10k each on Kalshi+Polymarket every 15–45 min, 57 days. Kalshi returns −16% to −30.8%. Research volume ≈ uncorrelated with outcome.
- **PolyBench** (arXiv 2604.14199) — contamination-proof benchmark; 38,666 binary Polymarket snapshots + synchronized CLOB + real-time news; 7 SOTA LLMs, 36,165 predictions (Feb 6–12 2026). Metrics: directional accuracy, Confidence-Weighted Return (CWR), APY, Sharpe via order-book sim.

### C) Professional whales — mostly HUMAN conviction, not bots (important nuance)
- **Théo / "Fredi9999" / "Theo4"** — French ex-Wall-Street trader. Deployed ~$85M on Trump in 2024 election markets; won ~$48–85M. Most profitable single political trade in prediction-market history. His edge was a *thesis* (argued public polls were biased; used "neighbor"/shy-voter polling logic) — a contrarian fundamental bet, NOT an LLM auto-research loop. Wallet 0x56687bf447db6ffa42ffe2204a05edaa20f55839 ("Theo4") = #1 all-time, $22,053,934 realized, 14 bets, ~$19 in losses. (polytrackhq.app, cointrenches.io)
- **Domer** — "World's #1 prediction markets trader"; finds edge on global political events via speed + discipline + manual research (ethankho.substack.com interview).
- Empirical edge studies: **Polyloly** ROI analysis of 219k whale trades (polyloly.com); **0xinsider** top-10 earners breakdown (0xinsider.com).

---
## 2. EXPERIMENTS people have run (from practitioner threads)
- **Cross-venue + 15-min crypto arbitrage** — the dominant open-source strategy (all repos above). r/PredictionsMarkets "Reverse Engineering the 10k a day Polymarket Bot" (▲91) reverse-engineers a 15-min crypto-market arb bot.
- **Event-driven "shock-timing" / mean-reversion** — r/PredictionsMarkets "I built a +39% Kalshi bot to exploit World Cup market panics (full strategy + code)" (▲18). **REALITY CHECK:** a replicator (u/RockyRoadn) backtested it on 2022 Cup + 4 games → "**IT DOESN'T WORK!!**"; another flagged the GitHub as a coin-stealer. Lesson: shared "profitable" strategies rarely replicate; many are scams.
- **Agent-led wallet analytics (Karpathy-style)** — r/ClaudeAI "I wired Claude Code into a database of every Polymarket wallet via MCP" (▲1707): 1.3B trades / 2.7M wallets in Postgres + MCP, queried in plain English. Top insight in comments: "edge exists in that wallet ≠ you can copy it — you're always too late." (sharps rotate wallets)
- **Multi-AI fair-value ensembling** — r/PillarLab "Best AI for Prediction Market Trading 2026": ran one market through 7 AI systems → fair-value estimates spanned 42%–71%; the *spread* (disagreement) was the most useful output.
- **Pro strategy taxonomy** — r/PillarLab "How Professionals Use Prediction Markets: 6 Strategies": top-10 wallets made money 3 ways = (1) macro conviction on Fed decisions, (2) algorithmic HF market-making, (3) event-driven single-day opportunism; one wallet +$3.26M.
- **Efficiency debate** — r/PredictionMarkets "Is there actually an information edge left on Polymarket or has it been arbitraged away?"; r/MakingMoneyonKalshi "Why making money on Kalshi is hard" ("the price already knows" — markets near-efficient).

---
## 3. DATA SOURCES surfaced
- **Polymarket orderbook archive** — 10TB+ open dataset, continuously updating: https://archive.pmxt.dev/Polymarket (r/datasets ▲43)
- **py-clob-client** — Polymarket CLOB order signing (used by most bots)
- **Kalshi Trade API** (native REST/WebSocket) — used by the World Cup bot + arb bots
- **Polymarket Gamma / CLOB APIs**; on-chain via Polygon/Dune
- Wallet-tracking / leaderboard tools: PolyTrack (polytrackhq.app), Cointrenches, 0xinsider, Polyloly
- (Note: "Polymarket acquired Dome" referenced as a data-infra event)

---
## 4. Regulatory / risk overlay (recurring on Reddit, context not core)
Heavy 2026 insider-trading discourse: US Senators banned from prediction markets; Google employee charged ($1M bet on a search term); Special Forces soldier arrested ($400K on Maduro capture); NYT "dozens of bets show insider trading"; CFTC (Chair Michael Selig) deploying ML/AI surveillance to hunt insider trading. → Two implications for an algo trader: (a) some "edge" in these markets is literally illegal private info, not modelable; (b) resolution/oracle + regulatory risk is a real cost line.

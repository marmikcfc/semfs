# AI-Led & Algorithmic Trading on Prediction Markets (Polymarket & Kalshi)
### A first-principles deep-research report — census, experiments, the math, and the data
_Compiled 2026-06-14. Sources: Composio (Reddit + Exa/web), a parallel `deep-research` multi-agent harness (3-vote adversarial verification), and first-principles analysis. X/Twitter could not be queried (connected API account had depleted credits) — social-practitioner signal comes from Reddit + cited X links surfaced via web._

---

## 0. The one-paragraph truth

A prediction-market contract pays **$1 if an event happens, $0 if not**, so its price *is* the crowd's probability, and the only thing that makes you money is **buying when your probability estimate `q` exceeds the market price `p`** (or selling when `q < p`). That is the whole game. A lot of people are now pointing AI at it — but the **only real-capital benchmark of autonomous AI traders (Prediction Arena, 6 frontier models, 57 days) shows every model *losing* 16–31% on Kalshi**, and it explicitly found that *research volume had little correlation with returns* ([arXiv 2604.07355](https://arxiv.org/abs/2604.07355)). Meanwhile **~67% of Polymarket profit accrues to the top 0.1% of accounts** (WSJ analysis), and the single biggest winner in history — France's "Théo" (~$48–85M on Trump 2024) — won with a **human contrarian thesis, not an LLM loop**. The honest conclusion: there *is* durable edge, but it lives in **true arbitrage**, **genuine speed/information**, and **rare conviction against crowd bias** — not in "auto-research → guaranteed profit." The way to "always make money at the end of the day" is not one magic bet; it is a **portfolio of many uncorrelated small edges + a risk-free arbitrage floor + Kelly-disciplined sizing**, which is a quant-fund design, not a prompt.

---

## 1. First principles — what these markets are, mathematically

### 1.1 The irreducible unit
A binary (YES/NO) contract is a **Bernoulli payoff**. Buy YES at price `p ∈ (0,1)`; receive `$1` if the event resolves YES, `$0` otherwise.

```
Edge per $1 staked on YES:   E[profit] = q − p
   q = YOUR probability estimate      p = market price (the crowd's probability)
Profitable  ⇔  q − p  >  fees + slippage + adverse-selection + resolution-risk
```

This splits the entire problem into two independent skills:
1. **Forecasting** — making `q` more accurate than `p` (where LLMs / "auto-research" live).
2. **Sizing & execution** — converting `q − p` into compounded bankroll growth without ruin (where Kelly / portfolio math live).

### 1.2 The menu — kinds of trading that actually happen
| # | Strategy | Edge source | Who does it |
|---|----------|-------------|-------------|
| 1 | **Directional / fundamental** | better forecast `q>p` | whales (Théo, Domer), forecasting researchers |
| 2 | **Market making** | earn bid–ask spread, manage inventory | pro HFT wallets, Polymarket LP-reward farmers |
| 3 | **Arbitrage** (≈risk-free) | YES+NO<$1; Σ(MECE outcomes)≠$1; cross-venue Poly↔Kalshi; logical `P(A∧B)≤P(A)` | the entire open-source bot crowd |
| 4 | **Statistical arb / relative value** | correlated markets diverge | quant desks |
| 5 | **Event-driven / latency** | news drops, book lags | speed bots, insiders |
| 6 | **Vol / time-decay** | binary-option gamma near resolution | options-literate traders |
| 7 | **Behavioral fade** | favorite-longshot & partisan bias | disciplined contrarians |

### 1.3 How they resolve — and the hidden risk line
- **Polymarket**: on-chain (Polygon), USDC-settled, resolved by the **UMA optimistic oracle** (proposer posts outcome → dispute window → token-holder vote). Risk: ambiguous wording, oracle disputes, slow finalization → **capital lock-up + tail manipulation risk**.
- **Kalshi**: **CFTC-regulated** US exchange, USD-settled, resolution per the contract's written rules by the exchange. Risk: contract-rule edge cases, regulatory/listing changes.
- Both now face a **2026 insider-trading crackdown** (US Senators banned; CFTC deploying ML surveillance; multiple arrests). Implication: some apparent "edge" is **illegal private information you cannot and should not model**, and resolution/regulatory risk is a real cost.

---

## 2. The census — how many folks, and who (with verification)

There are **three distinct camps**, and conflating them is the main way people get the "how many" question wrong.

### Camp A — Open-source bot builders (large, mostly arbitrage, mostly unproven)
Dozens of public repos; **the overwhelming majority implement the *same* idea**: cross-venue Polymarket↔Kalshi arbitrage and 15-minute crypto markets.

| Repo | What it does | Signal |
|------|--------------|--------|
| [Polymarket/agents](https://github.com/Polymarket/agents) | **Official Polymarket** framework: "Trade autonomously on Polymarket using AI Agents." Uses `py_clob_client` for CLOB execution + Gamma API for market data (verified at source-code level). MIT, ~3.7k★. | **The venue itself ships an agent framework** |
| [alsk1992/CloddsBot](https://github.com/alsk1992/CloddsBot) | Autonomous, Claude-primary + 8 LLM providers; 374★, 393 commits, v1.7.7. Trades **Polymarket + Kalshi only** (the "also Manifold/Metaculus/PredictIt" claim was **refuted** — README marks only Poly/Kalshi tradeable). | Real & active, but **README shows zero P&L proof** |
| [zostaff/poly-arbitrage-bot](https://github.com/zostaff/poly-arbitrage-bot) | Cross-exchange Poly↔Kalshi arb, paper+live, intra-market bundling | 22★ |
| [defi-ape/polymarket-kalshi-arbitrage-bot](https://github.com/defi-ape/polymarket-kalshi-arbitrage-bot) | 15-min arb, YES/UP spread + late-resolution gaps | — |
| [ImMike/polymarket-arbitrage](https://github.com/ImMike/polymarket-arbitrage) | Watches **10,000+ markets** for cross-platform inefficiency | — |
| [soldino777](https://github.com/soldino777/polymarket-arb-bot) · [456ape](https://github.com/456ape/kalshi-polymarket-arb) · [up2itnow0822](https://github.com/up2itnow0822/multi-clob-arb-scanner) · [zostaff/poly-trading-bot](https://github.com/zostaff/poly-trading-bot) | Cross-platform arb, "execute both legs, lock the spread" | low-star |
| [spfunctions/simplefunctions-cli](https://github.com/spfunctions/simplefunctions-cli) (`@spfunctions/cli`) | **MCP server + 42 commands**: queries live Kalshi+Polymarket orderbooks, exports JSON for **Claude Code / Codex** | The Karpathy-loop *tooling* layer |
| [simmer.markets](https://simmer.markets) | "**Prediction Markets for the Agent Economy**" — one API to trade Poly+Kalshi, self-custody wallets, safety rails, a "Skills" marketplace of strategies | Agent-trading as a *product category* |

> ⚠️ **Reality check (adversarial):** A viral r/PredictionsMarkets post — *"I built a +39% Kalshi World Cup bot (full strategy + code)"* — was independently replicated by a commenter who backtested it and reported **"IT DOESN'T WORK!!"**, while others flagged the linked GitHub as a **coin-stealer**. The dominant cynical-but-correct community take: *"if you had a real money-printing pattern, why post it on Reddit?"* ([thread](https://www.reddit.com/r/PredictionsMarkets/comments/1u3rn8s/)). Treat star counts and README claims as marketing until there is an on-chain wallet proving P&L.

### Camp B — Academic / research camp (small, rigorous, growing fast)
A clear research line evaluating whether LLMs can forecast and trade, ending in **real-capital benchmarks**:

| Work | Finding |
|------|---------|
| **Halawi et al. 2024**, "Approaching Human-Level Forecasting with LMs" (NeurIPS; [arXiv 2402.18563](https://arxiv.org/abs/2402.18563)) | LLM system Brier **.179 vs human crowd .149** → *approaches but does not beat* the crowd |
| **Wisdom of the Silicon Crowd** ([arXiv 2402.19379](https://arxiv.org/abs/2402.19379)) | An **ensemble** of LLMs rivals human-crowd accuracy |
| **ForecastBench** (Karger et al., ICLR 2025; [arXiv 2409.19839](https://arxiv.org/abs/2409.19839)) | Dynamic, contamination-resistant benchmark of forecasting; LLMs still below superforecasters |
| **Metaculus AI Benchmark** (2025); [Evaluating LLMs vs Expert Forecasters](https://arxiv.org/abs/2507.04562) | Gap to top humans narrowing through 2025 |
| **AIA Forecaster** ([arXiv 2511.07678](https://arxiv.org/abs/2511.07678)); **Agentic Forecasting w/ Sequential Bayesian Updating** ([arXiv 2604.18576](https://arxiv.org/abs/2604.18576)) | Agentic + explicit Bayesian-update architectures |
| **PolyBench** ([arXiv 2604.14199](https://arxiv.org/abs/2604.14199)) | 38,666 Polymarket snapshots + synced CLOB + news; 7 LLMs, 36k predictions; metrics incl. Confidence-Weighted Return, APY, Sharpe |
| **Prediction Arena** ([arXiv 2604.07355](https://arxiv.org/abs/2604.07355)) | **6 frontier models, real $10k each, 57 days → Kalshi returns −16% to −30.8%; research volume ≈ uncorrelated with profit** |

### Camp C — Professional whales (tiny, dominant, mostly *human*)
- **Théo / "Fredi9999" / "Theo4"** — French ex-Wall-Street trader; ~$85M deployed on Trump 2024, ~$48–85M profit; #1 all-time wallet `0x5668…5839` ($22M realized, ~$19 lifetime losses). Edge = a **contrarian thesis** that public polls under-counted Trump (neighbor/shy-voter logic), *not* an AI loop ([PolyTrack](https://www.polytrackhq.app/blog/polymarket-french-whale-case-study), [Cointrenches](https://cointrenches.io/fredi9999-polymarket-trump-election-whale-85m)).
- **Domer** — "world's #1 prediction-markets trader," edge from **speed + discipline + manual research** on global political events ([interview](https://ethankho.substack.com/p/how-the-worlds-1-prediction-markets-a07)).
- **Empirical edge studies:** [Polyloly](https://polyloly.com/blog/where-polymarket-edge-lives-cohort-roi-analysis) (ROI of 219k whale trades); [0xinsider](https://0xinsider.com/research/polymarket-highest-earners-top-10) (top-10 wallets = $96.5M ≈ 21% of all profit, held by ~0.07% of accounts).

**So "how many folks have done Karpathy-style auto-research on Polymarket"?** The *tooling* is now mainstream (Polymarket's own agent framework, CloddsBot, simmer.markets, simplefunctions MCP, plus dozens of arb bots and a Claude-Code-over-1.3B-trades analytics demo with 1.7k upvotes on r/ClaudeAI). But **publicly-verified, profitable, autonomous auto-research loops are essentially zero** — the one rigorous real-capital test lost money, and the big human winners didn't use them.

---

## 3. The experiments people ran — and what actually happened

- **Arbitrage (the crowd's default).** YES+NO<$1, Dutch books on MECE markets, cross-venue Poly↔Kalshi, 15-min crypto. Genuinely ≈risk-free *if* both legs fill and resolution is unambiguous — but **capacity-limited** (spreads close fast) and exposed to **leg risk** (one side fills, the other moves) and **resolution-timing** mismatches between venues.
- **Event-driven / latency.** Trade the seconds after news before the book reprices (r/PillarLab documents 40-minute repricings around the 2025 Iran strike). This is where speed + real-time feeds win — and where insiders illegally win.
- **Market making.** Quote both sides, harvest spread, manage inventory and **adverse selection** (you get filled exactly when you're wrong). Polymarket's LP-reward program subsidizes this.
- **Behavioral fade.** Favorite-longshot bias (longshots overpriced) and partisan/wishful pricing create systematic, fadeable mispricings.
- **Real-capital AI trading.** Prediction Arena (−16% to −31%) and PolyBench show current LLMs are **mediocre autonomous traders** even when they're decent forecasters — the gap is **monetizing** a correct view through sizing, timing, and microstructure, not generating the view.
- **Copy-trading.** Repeatedly fails: sharps **rotate wallets**, and by the time a winning trade is visible on-chain, *"you're always too late"* (top comment, r/ClaudeAI wallet-analysis thread).

---

## 4. The quant / math / probability toolkit — mapped to the system

| Principle | What it does | Honest caveat |
|-----------|--------------|---------------|
| **Edge `q − p`** | the only source of profit | needs calibration to be real, not imagined |
| **Kelly criterion** `f* = (q − p)/(1 − p)` | growth-optimal bet fraction for a YES at price `p` | use **¼–½ Kelly**: `q` is estimated with error; full Kelly + bad `q` ⇒ ruin |
| **Grinold's Fundamental Law** `IR ≈ IC·√Breadth` | **many uncorrelated small edges beat one big bet** — the case for automation | breadth is *fake* if bets are correlated |
| **Brier / log-loss / calibration** | the only honest scorecard for `q` | Halawi: LLM .179 vs crowd .149 — measure before you trust |
| **Bayesian updating** | fold new info into `q` in real time = the actual auto-research loop | garbage priors → confidently wrong |
| **No-arbitrage coherence** (YES+NO=1, Σ=1, `P(A∧B)≤P(A)`) | violations = free money | tiny, fleeting, capacity-bound |
| **Markowitz / shrinkage covariance** | correlation-aware sizing | **the anti-blow-up**; 20 election markets = 1 bet |
| **Kyle's λ / Glosten–Milgrom** | market-making & adverse-selection pricing | you are the dumb money until proven otherwise |
| **Optimal stopping** | hold-to-resolution vs sell early (capital-lockup cost) | UMA finalization can strand capital for weeks |
| **Extreme-value theory** | price longshots/tails the crowd misprices | thin data in the tail |

### Can you "always make money at the end of the day"?
Honestly: **only arbitrage is literally always-profit** (and it's small and competitive). Everything else is *probabilistically* profitable. The closest engineering approximation to "always" is:

```
   Risk-free floor (arbitrage)         ← guaranteed-ish, capacity-limited
 + Many uncorrelated +EV bets (LLN)    ← variance ∝ 1/N  ⇒  Sharpe ∝ √N
 + Fractional-Kelly sizing             ← maximize log-growth, bound ruin
 + Correlation-aware caps              ← stop the one-bet-in-20-hats blowup
 + Calibration feedback (Brier loop)   ← keep q honest, self-improve
 = positive expected log-growth with controlled drawdown
```

---

## 5. Data sources — the full taxonomy

**Market / price (the `p` side):**
- Polymarket **Gamma API** (`gamma-api.polymarket.com`) + **CLOB API** + `py-clob-client` (order signing)
- **Kalshi** REST/WebSocket Trade API (orderbook, trades, candlesticks)
- **On-chain**: Polygon subgraph, Dune Analytics; **pmxt 10TB+ open orderbook archive** ([archive.pmxt.dev/Polymarket](https://archive.pmxt.dev/Polymarket))
- Aggregators/leaderboards: Adjacent News, PolyTrack, Polyloly, 0xinsider, Metaforecast; sibling venues Betfair Exchange, Manifold, Metaculus

**Ground-truth / "smart-money anchor" (the `q` side), by domain:**
- **Sports** → **Pinnacle** (sharpest book), The Odds API, OddsPortal — the closing line is the benchmark `q`
- **Macro / rates** → FRED, BLS, **CME FedWatch**, SOFR futures
- **Elections** → 538 / **Silver Bulletin**, RealClearPolitics, state pollsters
- **Weather** → NOAA, ECMWF · **Crypto** → CEX prices, funding rates, on-chain

**Alternative / sentiment (early-signal):**
- **X/Twitter firehose, Reddit, Google Trends, Wikipedia pageviews**, GDELT, news APIs (NewsAPI, RavenPack), **on-chain whale-wallet tracking** (copy-signal, with the caveat above)

**Agent infrastructure (Karpathy-loop plumbing):**
- `Polymarket/agents`, `alsk1992/CloddsBot`, `simmer.markets` (one-API agent trading), `@spfunctions/cli` (MCP feed of live orderbooks into Claude Code/Codex)

---

## 6. The "infinite resources" design — a prediction-market quant fund

If money and compute were no object, you would **not** build "a bot." You'd build a **multi-desk fund with one shared belief engine**, because that's what the math (Grinold + Kelly + no-arbitrage) actually rewards:

```
                ┌─────────────────────────────────────────────┐
   DATA LAKE →  │  BAYESIAN BELIEF ENGINE  (produces calibrated │
  (all §5 feeds)│   q for every live market, every minute)      │
                └───────────────┬─────────────────────────────┘
                                │ q vs p  →  edge map
        ┌───────────────┬───────┴────────┬────────────────┐
        ▼               ▼                ▼                ▼
   ARB DESK        FORECAST DESK     MM / MICRO DESK    BEHAVIORAL DESK
   YES+NO, Σ,      breadth of many   quote spread,      fade longshot/
   cross-venue     small +EV bets    earn rebates       partisan bias
   (risk-free      (Grinold √N)      (Kyle λ aware)     (EVT tails)
    floor)
        └───────────────┴───────┬────────┴────────────────┘
                                ▼
              CORRELATION-AWARE FRACTIONAL-KELLY ALLOCATOR
              (shrinkage covariance; per-event & per-theme caps;
               reserve for UMA/oracle + regulatory risk)
                                ▼
              EXECUTION (CLOB APIs, leg-risk hedged, slippage-modeled)
                                ▼
              CALIBRATION LOOP  → Brier/log-loss every resolution →
              retrain priors, prune dead strategies  (the real auto-research loop)
```

Why this approximates "always profit": the **arb desk is a near-risk-free floor**, the **forecast desk converts breadth into √N variance reduction**, the **allocator prevents the correlation blow-up that kills everyone**, and the **calibration loop is the *only* legitimate version of "Karpathy-style auto-research"** — it improves `q` against measured Brier score instead of against vibes. Note what Prediction Arena teaches: **spend the compute on *better-calibrated q and better monetization/sizing*, not on raw "research volume,"** which was uncorrelated with returns.

---

## 7. Risks, limits & open questions (the via-negativa list)
- **Efficiency decay** — deep liquid markets (US-president topline) are already arbed; edge lives in niche/new/ambiguous/low-liquidity markets and the seconds after news.
- **The replication problem** — shared "profitable" strategies usually don't replicate (the +39% bot) and many repos are scams.
- **Oracle / resolution risk** — UMA disputes, ambiguous wording, weeks-long capital lock-up.
- **Regulation & insider info** — some "edge" is illegal private information; CFTC is now hunting it with ML; US-person access to Polymarket is restricted.
- **Capacity** — arbitrage and microstructure edges are small and competed away at scale.
- **The core empirical caution** — autonomous frontier-model trading *lost money* in the only real-capital test, and "more research" didn't help. The bottleneck is **calibration + monetization**, not idea generation.

---

## Appendix — primary sources
Academic: [Prediction Arena 2604.07355](https://arxiv.org/abs/2604.07355) · [PolyBench 2604.14199](https://arxiv.org/abs/2604.14199) · [Halawi 2402.18563](https://arxiv.org/abs/2402.18563) · [Wisdom of Silicon Crowd 2402.19379](https://arxiv.org/abs/2402.19379) · [ForecastBench 2409.19839](https://arxiv.org/abs/2409.19839) · [LLMs vs Expert Forecasters 2507.04562](https://arxiv.org/abs/2507.04562) · [AIA Forecaster 2511.07678](https://arxiv.org/abs/2511.07678) · [Agentic Bayesian Forecasting 2604.18576](https://arxiv.org/abs/2604.18576)
Code/infra: [Polymarket/agents](https://github.com/Polymarket/agents) · [CloddsBot](https://github.com/alsk1992/CloddsBot) · [simmer.markets](https://simmer.markets) · [simplefunctions-cli](https://github.com/spfunctions/simplefunctions-cli) · arb repos in §2
Practitioner/market: [PolyTrack French-whale](https://www.polytrackhq.app/blog/polymarket-french-whale-case-study) · [Cointrenches Théo4](https://cointrenches.io/theo4-polymarket-all-time-profit-leader-profile) · [Domer interview](https://ethankho.substack.com/p/how-the-worlds-1-prediction-markets-a07) · [Polyloly cohort ROI](https://polyloly.com/blog/where-polymarket-edge-lives-cohort-roi-analysis) · [0xinsider top-10](https://0xinsider.com/research/polymarket-highest-earners-top-10) · [pmxt 10TB archive](https://archive.pmxt.dev/Polymarket) · [MetaMask strategy primer](https://metamask.io/en-GB/news/advanced-prediction-market-trading-strategies)
_Verification note: claims above passed a 3-vote adversarial check or are first-principles derivations; the "+39% bot," the CloddsBot multi-venue claim, and "research volume drives returns" were specifically **refuted/qualified** during verification._

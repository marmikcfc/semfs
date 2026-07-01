> <!-- STALE-BANNER --> ⚠️ **STALE — not a semfs project doc** (2026-06-25). Skill/brainstorm scratch output; kept for history, not part of project documentation. Project state → [CURRENT_STATE.md](CURRENT_STATE.md).

# Build Small Hackathon — 10 GAN-Validated Ideas

**Generated:** 2026-06-07 · **Method:** GAN-style ideation (researcher → generator → blind discriminator), 10 rounds, 114 agents, ~4M tokens.
**Hackathon:** Build Small (HF + Gradio), hack window June 5–15 2026. Constraints: ≤32B params, Gradio app on a HF Space.

## How to read this
- **Convergence:** the loop did **NOT** reach "all 10 great simultaneously." It ran the full 10 rounds. The blind discriminator stayed adversarial — exactly the behavior a real GAN discriminator should show. Treat scores as a *tough* judge, not a friendly one.
- **`would_win` ideas** (discriminator believed they'd win their track at the final round): **#2, #4, #7, #10**, with **#5** at the edge.
- Scores are 0–10 per the **4 track criteria**. "great" = mean ≥ 8 AND min ≥ 7.

---

## Win-probability ranking

| Rank | # | Idea | Track | Mean | Awards stacked |
|------|---|------|-------|------|----------------|
| 1 | 4 | **Stockroom Eyes** | Backyard | 8.25 | OpenBMB · Tiny Titan · Off-the-Grid · Llama Champion · Best Demo · Field Notes |
| 2 | 2 | **Read-It-To-Me Mail** | Backyard | 8.0 | OpenBMB · Tiny Titan · Off-the-Grid · Best Demo · Field Notes |
| 3 | 7 | **Don't Let the AI Guess It** | TTW | 7.75 | OpenBMB · Tiny Titan · Off-the-Grid · Off-Brand · Best Demo |
| 4 | 10 | **Entropy Garden** | TTW | 7.75 | OpenAI gpt-oss · Off-Brand · Sharing-is-Caring · Field Notes |
| 5 | 5 | **Pillbox Watcher** | Backyard | 7.75 | Tiny Titan · Off-the-Grid · Llama Champion · Best Demo |
| 6 | 3 | **Nani's Recipe Time Capsule** | Backyard | 7.5 | Off-the-Grid · Off-Brand · Best Demo · Well-Tuned |
| 7 | 8 | **The Cartographer of Dreams** | TTW | 7.5 | OpenAI gpt-oss · FLUX · Modal · Off-Brand · Sharing-is-Caring |
| 8 | 1 | **Priya's First-Pass Reader** | Backyard | 7.25 | Off-the-Grid · Llama Champion · Best Demo · Field Notes |
| 9 | 9 | **Giuseppe Roasts Your Cart** | TTW | 7.0 | OpenBMB · Tiny Titan · Best Demo |
| 10 | 6 | **The Tide Pool Duel** | TTW | 7.0 | OpenAI gpt-oss · OpenBMB · Best Agent · Off-Brand · Sharing-is-Caring |

**If you build only one:** **#4 Stockroom Eyes** — highest score, lowest *conceptual* risk, densest non-padded award stack, and engineered around Backyard's top-weighted criterion (proof of repeated real use) rather than around novelty or polish.

---

## The meta-finding (most important)

The GAN **mode-collapsed onto one winning meta-pattern**: *"identify the single load-bearing risk, retire it on day 1 (or pre-hackathon), and make unfakeable evidence the spine of the submission."* It appears in 4/5 Backyard and all 5 TTW ideas.

This is a double-edged result:
- **Signal (good):** this *is* how you win a "did a real person use it / would you show a friend" hackathon. The judges reward evidence, not promises. The loop discovered the real objective function.
- **Mode collapse (watch out):** per the Novelty Playbook, optimizing every candidate to the same rubric-aware template is exactly the diversity failure NoveltyBench penalizes. The products differ; the *process* is homogeneous.

**Diversity check — ~6 truly distinct product engines across 10 ideas.** Redundancy clusters:
1. **Tiny-VLM-reads-a-physical-thing-aloud-offline-for-someone-vulnerable** → #2 (mail), #4 (shelf), #5 (pillbox). Same engine (MiniCPM-V/Moondream → deterministic check → Kokoro TTS, fully local), different object/persona. **Build at most 1–2** or they compete for the same judges.
2. **gpt-oss emergent-behavior-proven-on-day-1** → #6 (autonomous betrayal), #10 (token-starved decay). Same gamble, same award stack.
3. **Persona-reacts-to-your-input toys** → #9 (roasts your order), #7 (confident-wrong narrator). #7 is more original because *fallibility is the win condition*, not decoration.

**Orthogonal singletons** (least redundant): **#1** (text-only doc term-triage — no VLM/TTS/game) and **#8** (FLUX procedural cartography — the only image-gen/Modal entry).

---

## The 10 ideas in detail

### #4 — Stockroom Eyes *(Backyard · mean 8.25 · would_win)*  ⭐ build-one pick
**One-liner:** A kirana shopkeeper photographs his shelf at closing on his own phone and hears a spoken Hindi reorder list; the submission's spine is a real screen-recorded streak of 5–7 consecutive *unprompted* nights + a measured baseline-vs-after stockout delta.
**Model:** MiniCPM-V 4.6 (1.3B) on-device via llama.cpp + Kokoro TTS; reorder logic is a deterministic lookup (NO second LLM).
**Why small fits:** Coarse 3-class facing-state (full/low/empty) is the newly-viable mundane VLM job, robust at 1.3B; offline + Hindi + zero cost on a phone he already carries.
**Wow:** Open with "Ramesh had 4 stockouts last baseline week" → the timestamped 5-night unprompted streak → airplane-mode catch of Parle-G the night before it would have run out → post-tool stockouts now 1.
**Mechanism:** SCAMPER/TRIZ (substitute exact count → 3-class proxy; eliminate ceremonial LLM; reorder project so evidence is the spine).
**Biggest risk:** A tired shopkeeper photographing unprompted for 5–7 nights is the hardest behavior to secure; 48h on-device build is the most fragile engineering claim.
**10-day plan:** D1 baseline count + accuracy table; D2 ship thin on-device loop (≤48h); D3–9 field-study streak vs baseline + dashboard/replay viewer; D10 demo opening with the baseline number.

### #2 — Read-It-To-Me Mail *(Backyard · mean 8.0 · GREAT · would_win)*
**One-liner:** A low-vision neighbor presses one big physical button, holds a letter to his phone, hears "This is a bill from PG&E, $84, due June 20th"; inference runs on a small box in his home so bank/medical mail never leaves the house.
**Model:** MiniCPM-V 4.6 (1.3B) via local Ollama + OpenCV page-detect + Kokoro-82M TTS; Bluetooth shutter as the physical start affordance (not a wake word).
**Why small fits:** Transform-in-place OCR+classify runs honestly at 1.3B on a cheap home box — the only way mail stays private; audio-in/out fits a non-reading user.
**Wow:** Cloud Wi-Fi shown OFF; blindfold-tested, alone, across three separate solo days, he triages real mail without his daughter; day-1 start-success number on screen.
**Mechanism:** First-principles (strip "help with mail" to its irreducible blocker — can a blind user reliably *start* without sight? → physical button).
**Biggest risk:** Two demo-day moving parts a pure-Gradio app avoids — a tunnel to the home box + Ollama uptime on cheap hardware. True product risk is the *capture* step, not the VLM.
**10-day plan:** D1 stand up local VLM+TTS, wire shutter, MEASURE start-success; D2 page-detect + single VLM fire + confidence guard; D3 TTS + harden capture-retake; D4 public Space over tunnel; D5–9 film solo sessions across days; D10 demo leads with start-success number.

### #7 — Don't Let the AI Guess It *(TTW · mean 7.75 · would_win)*
**One-liner:** A party game where one friend secretly draws a word, winning only if a tiny vision model NEVER guesses it while the room watches the AI's confidently-wrong narration crumble; the pitch already carries a 30s Wizard-of-Oz clip proving a difficulty sweet-spot exists.
**Model:** MiniCPM-V 2.6 (~2B, Tiny Titan) top-3 guesses + Qwen2.5-1.5B confident-wrong rewriter, fully local.
**Why small fits:** A small VLM is fast enough to guess on a tight cadence (cloud too slow/costly), and its fallibility *is* the game — proven tunable to a balance band by the pre-run WoZ.
**Wow:** A friend draws "lighthouse" dodging the AI; it narrates "a candle… now a rocket… is that water?" as the confidence threshold tightens; partial-credit on "tower"; the room cheers when it ends still wrong.
**Mechanism:** SCAMPER + constraint injection; de-risked by running a zero-code WoZ *before* the hackathon.
**Biggest risk:** The WoZ used a *human* reading guesses — it validated the social loop & difficulty band but NOT that Qwen-1.5B autonomously generates the persona at quality/cadence. Real-time canvas + streaming is the untouched engineering risk.

### #10 — Entropy Garden *(TTW · mean 7.75 · would_win)*
**One-liner:** A creature's inner monologue runs on a literal "token budget" of warmth — fed, it muses in lush sentences; starving, its thoughts collapse to "cold. find. warm." The viewer drags a heat vent to drive the degrade→revive arc.
**Model:** gpt-oss-20b only (configurable reasoning-effort + per-creature token budget IS the mechanic).
**Why small fits:** gpt-oss's configurable reasoning effort and visible CoT are the exact knob the mechanic needs — you modulate the model's compute and the viewer sees the cognitive effect.
**Wow:** Viewer shuts the vent; "I wonder if the river remembers spring" collapses to "cold. find. warm." as the token meter drains; opens it and it reblooms.
**Mechanism:** Cross-domain analogical transfer (thermodynamics: free energy as budget for cognitive order) + constraint injection.
**Biggest risk:** The entire payload rides on one unanswered empirical question — does token-starved gpt-oss read as poignant or as *mush*? Honest day-1 gate: Branch A (emergent, lock breakpoints) vs Branch B (designed instinct-vocabulary, named on-screen as designed). The author's own example hints at Branch B.

### #5 — Pillbox Watcher *(Backyard · mean 7.75)*
**One-liner:** A post-cardiac mother photographs her weekly pill organizer in a phone-stand jig and gets a gentle spoken check; rides on real captured footage — a hardened daily loop, the longest unprompted streak, a no-photo nudge, and one genuine confirm-don't-assert catch of an ambiguous blood-thinner compartment.
**Model:** Moondream2 (1.9B VLM) + deterministic pattern-checker + Kokoro TTS (<2.5B total).
**Why small fits:** Full/empty reading on a fixed jigged layout is constrained classification a tiny VLM handles; dangerous cases route through an uncertainty-gated *question*, never a risky assertion. Health data is the canonical do-not-cloud case.
**Mechanism:** Inversion (design solely to avoid catastrophe / false-alarm / silent failure).
**Biggest risk:** Win condition depends on luck — a genuine near-miss must fire on camera in the window, and the streak may land at 3 days. 1.9B reading 14 cells under kitchen glare is the highest-risk component.

### #3 — Nani's Recipe Time Capsule *(Backyard · mean 7.5)*
**One-liner:** A grandmother narrates a dish in Gujarati while cooking and the app grows an offline voice-cookbook in her own words; the demo-critical interaction (fixing ASR errors) is locked to a guaranteed-buildable native-Gradio mechanic so it ships unconditionally.
**Model:** faster-whisper (large-v3) + Qwen2.5-7B; SDXL-Turbo header & Gujarati fine-tune are upside only. Fully local.
**Why small fits:** Voice-in is the right modality for a non-typing elder; ASR + structuring is a bounded transform; the one-button correction layer makes imperfect ASR yield a faithful card.
**Mechanism:** Conceptual blending (transcribe-and-structure × family-heirloom preservation).
**Biggest risk:** Backyard's first-weighted criterion is the soft spot — the *true* repeat user is a grandchild facilitator, not the elder; "preserve grandma's recipes" is an archetypal hackathon idea.

### #8 — The Cartographer of Dreams *(TTW · mean 7.5)*
**One-liner:** You describe last night's dream aloud and a local engine grows a persistent illustrated atlas of your subconscious; day-1 proves the make-or-break thing — two independent FLUX regions auto-joined by a *programmatically*-generated coastline that reads as geography.
**Model:** Whisper + gpt-oss-20b (extraction/graph) + Qwen2.5-3B (region prompts) + FLUX.1-schnell, on Modal serverless GPU.
**Why small fits:** Bounded extraction-to-schema + interactive image gen is the ≤32B sweet spot; FLUX is load-bearing.
**Mechanism:** Conceptual blending (dream journal × persistent procedural cartography).
**Biggest risk:** Highest ceiling, **lowest floor.** The wow is gated on an unproven technical core (FLUX gives no cross-generation consistency; auto-knitting two diffusion outputs is not a well-trodden technique). Honest fallbacks (inpainting / curated journal) are materially less impressive. 5 components + custom frontend in 8 days is heavy.

### #1 — Priya's First-Pass Reader *(Backyard · mean 7.25)*
**One-liner:** A freelancer drag-drops any inbound client doc before replying and instantly sees risky terms flagged against her own norms; repeat clients also get a verbatim "what moved since last time" diff.
**Model:** Qwen2.5-7B-Instruct (GGUF via llama.cpp), single model, no embedder.
**Why small fits:** Locate-and-quote of a fixed risky-term set over one short doc + compare to a stored norm table is squarely in 7B reliability territory; confidential rate/IP terms are exactly what a freelancer refuses to upload.
**Mechanism:** Inversion + JTBD (make the *frequent* first-pass the spine; demote the rare renewal-diff to a bonus).
**Biggest risk:** Reads as a competent clause-extractor — useful but not surprising. 7B GGUF on free Spaces CPU will feel sluggish, in tension with its own privacy story.

### #9 — Giuseppe Roasts Your Cart *(TTW · mean 7.0)*
**One-liner:** A travel toy where one opinionated Roman guide doesn't just translate the Italian menu you photograph — he reacts to the dishes you actually pick, roasting clashing combinations ("Cappuccino AFTER lunch? With the carbonara? We are not friends.").
**Model:** MiniCPM-V 4.6 (1.3B) + Qwen2.5-3B + Kokoro (~3.5B total).
**Why small fits:** Offline OCR + reasoning over a multi-dish selection to produce one grounded combination-verdict is a bounded multimodal transform; the verdict depends on the dishes *you* selected on an unseen menu — creation, not retrieval.
**Mechanism:** SCAMPER (Modify + Combine) — change the verb from "translate each dish" to "judge the combination you selected."
**Biggest risk:** Originality is the cap (worn "opinionated AI roasts your choices" format); the funniest roasts are memorized cultural tropes — a skeptical judge sees the gap between wow and claimed mechanic. Whole demo rides on 1.3B VLM OCR of a judge's badly-lit menu.

### #6 — The Tide Pool Duel *(TTW · mean 7.0)*
**One-liner:** Two tide-pool creatures, each run by a *different* lab's small model, make binding public pledges over a shrinking pool as the tide goes out; a pledge is visibly kept or broken.
**Model:** gpt-oss-20b (schemer crab) + MiniCPM3-4B (trusting anemone), total ≤24B.
**Why small fits:** Small-model speed makes a live 2-agent duel affordable per turn; the heterogeneous pairing turns ≤32B into the aesthetic (each creature "thinks differently" because it literally is a different model).
**Mechanism:** Cross-domain analogical transfer (Ostrom common-pool-resource game + game-theoretic promise-breaking, falling tide = depletion clock).
**Biggest risk:** Most fragile core bet, and its own rules forbid the easy fix: small models often produce betrayals that read as *incoherence*, not felt treachery. Crowded genre (multi-agent LLM social games). Delight is cold and watch-only.

---

## Score trajectory across the 10 GAN rounds
(greatCount = # ideas hitting mean≥8 & min≥7 that round)

| Round | great | Notable |
|-------|-------|---------|
| 1 | 0 | cold start; means 6.5–7.75 |
| 2 | 2 | #3, #10 hit great |
| 3 | 0 | discriminator re-failed everything (adversarial swing) |
| 4 | 1 | #10 |
| 5 | 1 | #10 |
| 6 | 2 | #5, #10 |
| 7 | 1 | #5 |
| 8–10 | varies | never all-10; stopped at cap |

The oscillation (great count rising then collapsing) is the GAN signature — the generator chases the boundary, the discriminator moves it. No Nash equilibrium where all 10 satisfy a tough judge was reached in 10 rounds.

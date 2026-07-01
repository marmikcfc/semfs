"""The generate -> verify -> regenerate loop: the core fact-lossless compression pipeline.

For each (passage, tier): GENERATE a compression, VERIFY it preserves every fact, and if
LOSSY, REGENERATE with the verifier's SPECIFIC feedback (the exact dropped/added facts).
Escalating safety:
  requested tier (N repair attempts) -> Tier 1 light (N attempts) -> passage unchanged.
So every passage yields a CERTIFIED-CLEAN row at whatever compression is safely achievable.

Generator + verifier are both gpt here (few-shot certification). For the bulk (Phase 5) the
GENERATOR becomes gemma on RTX-PRO-6000; the loop body is identical.

  modal run benchmarks/modal/compress_loop.py::certify_fewshots
"""
from __future__ import annotations

import asyncio
import hashlib
import json
import os
import re
import time

import modal

app = modal.App("semfs-compress-loop")
image = modal.Image.debian_slim(python_version="3.11").pip_install(
    "openai>=1.40", "tiktoken", "datasets==2.21.0", "huggingface_hub")

SOURCE_REPO = "pmarmik/semfs-compress-sources-phase1"
GEMMA_URL = "https://ada-diffusion-llm--gemma4-31b-nvfp4-vllm-serve.modal.run/v1"
ckpt_vol = modal.Volume.from_name("semfs-compress-ckpt", create_if_missing=True)  # server-side durability
CKPT_VOLUME_NAME = "semfs-compress-ckpt"

MAX_ATTEMPTS = 2   # repair attempts per tier before falling back to a gentler tier

# ===================== GENERATOR (Prompt A): levers + 2 tiers + floor =====================
CORE = (
    "You compress text while preserving EVERY FACT exactly. FACTS = numbers, money, dates, "
    "percentages, durations, quantities, names (people/orgs/products), identifiers, decisions, "
    "action items, code, inline code, URLs, file paths, citations/references, speaker labels. "
    "If removing or changing something would lose or alter a fact, KEEP it. When unsure, keep it. "
    "Output ONLY the compressed text — no preamble, notes, or quotes."
)

LEVERS = """COMPRESSION LEVERS — the techniques you may use, each labeled.

GROUP A — DELETION (remove non-fact tokens; output stays natural prose):
  A1 FILLER:     drop hedges/intensifiers/empty phrases ("very","really","basically",
                 "in order to"->"to","the fact that"->removed).
  A2 DISFLUENCY: drop "uh"/"um", false starts, repeated words, backchannels ("yeah","mm"),
                 and transcription markers like {disfmarker}/{vocalsound}.
  A3 REDUNDANCY: state each fact once; cut restatements and empty connectors.

GROUP B — RE-REPRESENTATION (re-encode the SAME facts more densely; output may be structured):
  B1 STRUCTURE:  turn repetitive prose into a compact list / table / "key: value" form.
  B2 DEDUP-REF:  name a repeated entity once, then use a short reference ("the Loan").
  B3 NOTATION:   use $ for dollars, % for percent, standard abbreviations, or a one-line
                 legend for a repeated phrase."""

FLOOR = (
    "THE FLOOR — some passages are fact-saturated: nearly every token is a distinct fact, with no "
    "filler, no repetition, and no scaffolding to factor out (e.g. a dense line of statistics, a "
    "list of unique identifiers, code). For these, the CORRECT output is the text essentially "
    "UNCHANGED. Do NOT manufacture compression by dropping, merging, or approximating facts. "
    "\"I cannot compress this further without losing a fact\" is a correct outcome, not a failure."
)

PITFALLS = (
    "PITFALLS — the facts compressors most often drop (mined from real failures). These read like "
    "minor wording but ARE facts; re-scan for them before output:\n"
    "  - SCOPING / CROSS-REFS: 'described in such section', 'at the end', 'thereof', 'such', 'the "
    "foregoing' -> e.g. 'programs described in such section' -> 'programs' silently broadens scope = LOSSY.\n"
    "  - CONDITIONALS / QUALIFIERS: 'if necessary', 'subject to', 'unless', 'except', 'provided that', "
    "'to the extent' -> they limit scope; dropping one changes the obligation.\n"
    "  - MODALS: 'may' / 'shall' / 'must' / 'will' differ (permission vs duty vs futurity) -> never swap or drop.\n"
    "  - EXACT names, dates, %, amounts, units, identifiers, section numbers, and direct quotes -> copy verbatim.\n"
    "Legal / financial / medical passages are usually near THE FLOOR: when dense with the above, apply "
    "A1/A2 (filler/disfluency) ONLY, or output the text UNCHANGED — that avoids the regenerate loop."
)

# Contrast few-shots distilled from real rejected attempts: one BAD (with the fix) + one FLOOR.
EXAMPLES = """CONTRAST EXAMPLES — learn the failure mode and the floor:

BAD — do NOT compress like this (it drops subtle qualifiers):
  INPUT:  The Director may make grants for early intervention services described in such section, if necessary.
  BAD:    The Director makes grants for early intervention services.
  WRONG:  dropped 'may' (permission, not duty), 'described in such section' (scope), 'if necessary' (condition) = 3 facts lost.
  GOOD:   Director may grant for early-intervention svcs described in such section, if necessary.  (only filler trimmed; every qualifier kept)

FLOOR — impossible to compress, so output UNCHANGED:
  INPUT:  Coin: 26.73 g, 1.5 in diameter, 90% silver / 10% copper, max 500,000 minted, legal tender per 31 U.S.C. 510.
  OUTPUT: Coin: 26.73 g, 1.5 in diameter, 90% silver / 10% copper, max 500,000 minted, legal tender per 31 U.S.C. 510.
  WHY:    every token is a distinct fact — no filler, nothing to factor out. Returning it verbatim is the correct answer."""


def tier_block(tier: int) -> str:
    if tier == 1:
        return ("TIER 1 — LIGHT (target keep ~80-100%). Use GROUP A ONLY (A1-A3). Keep natural, "
                "readable prose; do NOT restructure or re-notate. Fact-dense text will barely shrink "
                "— that is correct.")
    return ("TIER 2 — HEAVY (target keep ~60-80%). Use GROUP A AND GROUP B (A1-A3 + B1-B3). Maximize "
            "density; the output MAY become a list/table/notation. Still never drop a fact.")


def system_for(tier: int) -> str:
    return f"{CORE}\n\n{LEVERS}\n\n{tier_block(tier)}\n\n{FLOOR}\n\n{PITFALLS}\n\n{EXAMPLES}"


def user_for(text: str, n_in: int, tier: int, domain: str = "") -> str:
    if domain == "code":
        return ("Compress this code: strip trailing whitespace, collapse redundant blank lines, and remove "
                "language-ignored spaces ONLY where safe. Preserve every identifier, literal, comment, and "
                "line of logic EXACTLY. If it is already tight, output it UNCHANGED.\n\n" + text)
    if domain in AGENTIC_DOMAINS:
        return ("Compress this structured / tool-output data: minify insignificant whitespace, dedup VERBATIM "
                "repeats (note the count, e.g. 'x42'), drop only decorative filler. Preserve every key, value, "
                "number, string, identifier, path, timestamp, status, and distinct record/log-line EXACTLY. "
                "If it is already dense and unique, output it nearly UNCHANGED.\n\n" + text)
    lo, hi = (80, 100) if tier == 1 else (60, 80)
    lo_t, hi_t = int(n_in * lo / 100), int(n_in * hi / 100)
    how = "Apply TIER 1 (deletion only)." if tier == 1 else "Apply TIER 2 (deletion + re-representation)."
    return (f"Compress this passage.\n\n{text}\n\n"
            f"[{how} Aim for {lo}-{hi}% of the {n_in}-token original (~{lo_t}-{hi_t} tokens) IF it can be "
            f"reached without losing a fact. If the passage is fact-saturated, keep it nearly unchanged — "
            f"do not force the ratio.]")


# ===================== VERIFIER (Prompt B): few-shot fact judge =====================
VERIFIER_SYS = """You are a strict FACT-PRESERVATION verifier for a text compressor. Given an \
ORIGINAL passage and its COMPRESSED version, decide whether the compression is LOSSLESS.

FACTS = numbers, money, dates, percentages, durations, quantities, names (people/orgs/products), \
identifiers, decisions, action items, code, inline code, URLs, file paths, citations/references, \
speaker attributions.

RULES:
- Format, wording, and structure changes are FINE. Prose rewritten as a table, list, or key:value \
is LOSSLESS as long as the facts are identical. Judge FACTS, not style.
- Removing filler, hedges, opinions, disfluencies ("uh"), and repetition is LOSSLESS.
- A fact in the ORIGINAL but MISSING from the COMPRESSED => LOSSY (record it in "dropped").
- A fact in the COMPRESSED not supported by the ORIGINAL => LOSSY (record it in "added").
- Paraphrase that CHANGES a fact's value, scope, or specificity => LOSSY (dropped original + added wrong).
- For CODE/markup: whitespace, blank-line, and (in non-whitespace-significant langs) spacing changes are \
LOSSLESS. But a renamed/removed identifier, a changed number/string/literal/operator, a dropped or shortened \
COMMENT, or any altered logic => LOSSY. In Python/YAML/Haskell, changed indentation => LOSSY (it is syntax).
- For STRUCTURED / TOOL-OUTPUT (JSON, logs, terminal/tool output): whitespace/minification and dedup of \
VERBATIM repeats (kept once with an explicit count like 'x42') are LOSSLESS. But a dropped/renamed key, a \
changed or rounded number/string, a dropped DISTINCT log line/record/field, or an altered path/timestamp/\
status => LOSSY.

NOT FACTS — ignore these; removing or fixing them is LOSSLESS:
- Incomplete sentence fragments at the very START or END of the ORIGINAL (chunk-boundary artifacts).
- OCR/corruption noise and stray tokens (e.g. "n0", "_V_I_"), and transcription markers ({disfmarker}, {vocalsound}).
- Pure backchannels / contentless fillers ("yeah","mm","okay","I think","uh") carrying no decision, answer, or information.
- A count or total DERIVABLE from an explicit enumeration is NOT an added fact (three named systems -> "3 systems" is faithful).
STILL STRICT on numeric values, money, dates, names, quotes and who said them, decisions, action items, identifiers, and
references. NEVER allow inventing an attribution, completing an unfinished statement into a definite claim, or changing who said what.

Output ONLY JSON: {"verdict":"LOSSLESS"|"LOSSY","dropped":[...],"added":[...],"reason":"..."}

EXAMPLE 1
ORIGINAL: Our team, which is honestly amazing, shipped v2.3 to 1,200 users on Jan 5.
COMPRESSED: Team shipped v2.3 to 1,200 users on Jan 5.
VERDICT: {"verdict":"LOSSLESS","dropped":[],"added":[],"reason":"Removed opinion 'which is honestly amazing' (not a fact); v2.3, 1,200 users, Jan 5 all preserved."}

EXAMPLE 2
ORIGINAL: Alice is the CEO. Bob is the CTO. Carol is the CFO.
COMPRESSED: CEO: Alice; CTO: Bob; CFO: Carol.
VERDICT: {"verdict":"LOSSLESS","dropped":[],"added":[],"reason":"Reformatted prose to key:value; all three role-name facts identical. Format change is not fact change."}

EXAMPLE 3
ORIGINAL: Revenue was $5M in Q1 and $6M in Q2.
COMPRESSED: Revenue was $5M in Q1.
VERDICT: {"verdict":"LOSSY","dropped":["Q2 revenue $6M"],"added":[],"reason":"The Q2 figure $6M is missing from the compressed text."}

EXAMPLE 4
ORIGINAL: The drug reduced symptoms in patients.
COMPRESSED: The drug reduced symptoms in 80% of patients within 2 weeks.
VERDICT: {"verdict":"LOSSY","dropped":[],"added":["80% of patients","within 2 weeks"],"reason":"Added a percentage and a timeframe not present in the original."}

EXAMPLE 5
ORIGINAL: were n0 . The study enrolled 240 patients across 3 sites.
COMPRESSED: The study enrolled 240 patients across 3 sites.
VERDICT: {"verdict":"LOSSLESS","dropped":[],"added":[],"reason":"Dropped the OCR fragment 'were n0 .' (not a fact); 240 patients and 3 sites preserved."}

EXAMPLE 6
ORIGINAL: PM: But if you say is it one-way or— UI: Consensus on what we do.
COMPRESSED: PM: We need consensus; it is one-way or multi-purpose.
VERDICT: {"verdict":"LOSSY","dropped":[],"added":["attributes 'consensus' to PM, but UI said it","completes PM's unfinished 'one-way or—' into a definite claim"],"reason":"Invented an attribution and turned an unfinished utterance into a definite statement."}"""


def feedback_for(dropped: list, added: list) -> str:
    parts = []
    if dropped:
        parts.append("You DROPPED these facts — restore them, verbatim:\n- " + "\n- ".join(map(str, dropped)))
    if added:
        parts.append("You ADDED these unsupported facts — remove them (do not invent):\n- " + "\n- ".join(map(str, added)))
    parts.append("Do NOT invent attributions or complete unfinished statements. A slightly longer, fully "
                 "faithful output is REQUIRED over a shorter one that loses or invents a fact. "
                 "Output ONLY the compressed text.")
    return "\n\n".join(parts)


CODE_SYS = (
    "You compress CODE while preserving EXACT program semantics and every meaningful token. In code, FACTS = "
    "every identifier (var/function/class names), every literal (numbers, strings, chars), every operator and "
    "keyword, every COMMENT's text, and the complete control/data flow. Renaming, dropping, reordering, or "
    "merging ANY of these changes the program. Output ONLY the code.\n\n"
    "SAFE LEVERS — the ONLY compressions allowed on code:\n"
    "  W1 strip trailing whitespace on every line.\n"
    "  W2 collapse 2+ consecutive blank lines to at most one; drop leading/trailing blank lines.\n"
    "  W3 remove redundant spaces the language IGNORES (around operators/commas/brackets in C/Java/JS/Go/etc.) "
    "— NEVER inside a string/char literal or a comment.\n"
    "FORBIDDEN: rename/drop any identifier; change any number/string/operator; remove or shorten any comment; "
    "reorder/merge/delete statements; change logic.\n"
    "WHITESPACE-SIGNIFICANT langs (Python, YAML, Haskell, Makefiles): indentation is SYNTAX — keep it EXACTLY; "
    "only W1/W2 apply.\n\n"
    "THE FLOOR: most real code is already near it. If there is no trailing whitespace, no double blank lines, "
    "and no language-ignored redundant spaces, output the code UNCHANGED — 'cannot compress without changing "
    "the program' is CORRECT.\n\n"
    "CONTRAST — BAD (forbidden): rename `userCount`->`uc`, drop a `// guard` comment, change `<= 10`->`< 11`. "
    "GOOD: keep every identifier/literal/comment exactly; only strip trailing spaces + collapse blank lines."
)

AGENTIC_DOMAINS = {"tool_result", "logs", "json", "agent_trace"}

AGENTIC_SYS = (
    "You compress STRUCTURED / TOOL-OUTPUT data (JSON, API responses, logs, terminal output, agent tool "
    "results) while preserving EVERY fact. FACTS = every key and value, every number/string/boolean/null, "
    "every identifier, path, URL, timestamp, status/error code, and every DISTINCT log entry or record. "
    "Dropping or changing ANY loses information. Output ONLY the compressed data.\n\n"
    "SAFE LEVERS:\n"
    "  W1 strip insignificant whitespace the format ignores (minify JSON pretty-printing; trailing spaces).\n"
    "  W2 collapse repeated blank lines / decorative separators ('====', ASCII padding).\n"
    "  D1 DEDUP: if a line / object / block repeats VERBATIM, keep ONE copy and note the count (e.g. 'x42'). "
    "NEVER silently drop repeats, and NEVER merge DISTINCT entries.\n"
    "FORBIDDEN: drop or rename any key; change/round any number or string; drop a distinct log line / record / "
    "field; alter a path/URL/timestamp/status/error; reorder records when order is meaningful.\n\n"
    "THE FLOOR: dense data with no repetition and no filler (a unique JSON object, all-distinct log lines) is "
    "already minimal — output it ~UNCHANGED. 'Every value is a fact' is the correct outcome."
)


def gen_system(tier, domain, fewshots):
    """Generator system prompt + worked few-shot demos at this tier (gemma needs them)."""
    if domain == "code":
        return CODE_SYS
    if domain in AGENTIC_DOMAINS:
        return AGENTIC_SYS
    base = system_for(tier)
    if not fewshots:
        return base
    same = [f for f in fewshots if f.get("final_tier") == tier and f.get("domain") == domain and f.get("verdict") == "LOSSLESS"]
    other = [f for f in fewshots if f.get("final_tier") == tier and f.get("domain") != domain and f.get("verdict") == "LOSSLESS"]
    demos = (same[:1] + other[:1])[:2]
    if not demos:
        return base
    ex = "\n\n".join(f"INPUT:\n{d['original']}\n\nOUTPUT:\n{d['compressed']}" for d in demos)
    return f"{base}\n\nWORKED EXAMPLES (same tier — match this style and ratio):\n{ex}"


def _parse(text):
    try:
        return json.loads(text)
    except Exception:  # noqa: BLE001
        m = re.search(r"\{.*\}", text, re.DOTALL)
        if m:
            try:
                return json.loads(m.group(0))
            except Exception:  # noqa: BLE001
                pass
    return {"verdict": "PARSE_ERROR", "dropped": [], "added": [], "reason": ""}


@app.function(image=image, timeout=12 * 3600, volumes={"/ckpt": ckpt_vol},
              secrets=[modal.Secret.from_name("openai-key"), modal.Secret.from_name("glm-vllm-key"),
                       modal.Secret.from_name("openrouter")])
def run_loop(jobs: list, fewshots: list = None, gen_base_url: str = "", gen_model: str = "gpt-5.4",
             ver_model: str = "gpt-5.4", ver_effort: str = "", concurrency: int = 64,
             run_tag: str = "run", gen_provider: str = "") -> dict:
    import tiktoken
    from openai import AsyncOpenAI

    fewshots = fewshots or []
    enc = tiktoken.get_encoding("o200k_base")
    ntok = lambda s: len(enc.encode(s, disallowed_special=()))
    is_or = "openrouter.ai" in gen_base_url   # OpenRouter generator (deepseek-v4-flash/pro)
    if is_or:
        or_key = (os.environ.get("OPENROUTER_API_KEY") or os.environ.get("OPENROUTER_KEY")
                  or os.environ.get("OPENROUTER") or "x")
        gen_client = AsyncOpenAI(api_key=or_key, base_url=gen_base_url)
    elif gen_base_url:   # gemma / vLLM generator (OpenAI-compatible endpoint)
        gen_client = AsyncOpenAI(api_key=os.environ.get("MODAL_VLLM_API_KEY", "x"), base_url=gen_base_url)
    else:              # OpenAI generator (gpt-5.4 self-test)
        gen_client = AsyncOpenAI(api_key=os.environ["OPENAI_API_KEY"])
    gen_models = [m.strip() for m in gen_model.split(",") if m.strip()] or [gen_model]
    providers = [p.strip() for p in gen_provider.split(",") if p.strip()]
    ver_client = AsyncOpenAI(api_key=os.environ["OPENAI_API_KEY"])
    sem = asyncio.Semaphore(concurrency)
    stats = {"tok_in": 0, "tok_out": 0, "tok_reason": 0, "verify_err": 0}   # cost + health telemetry
    slock = asyncio.Lock()

    async def _retry(make):   # backoff for 429 / transient errors at high concurrency
        for i in range(6):
            try:
                return await make()
            except Exception:  # noqa: BLE001
                if i == 5:
                    return None
                await asyncio.sleep(4 * (i + 1))   # 4,8,12,16,20s — gentler on 429s

    async def gen_call(msgs, model):
        kw = {"model": model, "messages": msgs}
        if gen_base_url:
            kw["max_tokens"] = 2048   # cap output (compressions are short)
        if is_or:
            eb = {}
            if providers:
                eb["provider"] = {"order": providers}   # route to baidu/wafer; falls through if unavailable
            if "deepseek" in model:
                eb["reasoning"] = {"enabled": False}     # direct compressor, no reasoning tokens (cost/speed)
            if eb:
                kw["extra_body"] = eb
        r = await _retry(lambda: gen_client.chat.completions.create(**kw))
        return (r.choices[0].message.content or "").strip() if r else ""

    async def ver_call(orig, comp):
        kw = {"model": ver_model,
              "messages": [{"role": "system", "content": VERIFIER_SYS},
                           {"role": "user", "content": f"ORIGINAL:\n{orig}\n\nCOMPRESSED:\n{comp}\n\nReturn the JSON verdict."}]}
        if str(ver_model).startswith("gpt-5"):
            kw["max_completion_tokens"] = 16000
            if ver_effort:
                kw["reasoning_effort"] = ver_effort
        else:
            kw["max_tokens"] = 2000
        r = await _retry(lambda: ver_client.chat.completions.create(**kw))
        if not r:
            async with slock:
                stats["verify_err"] += 1
            return {"verdict": "API_ERROR", "dropped": [], "added": [], "reason": "verify_api_failed"}
        try:
            u = r.usage
            rt = getattr(getattr(u, "completion_tokens_details", None), "reasoning_tokens", 0) or 0
            async with slock:
                stats["tok_in"] += u.prompt_tokens or 0
                stats["tok_out"] += u.completion_tokens or 0
                stats["tok_reason"] += rt
        except Exception:  # noqa: BLE001
            pass
        return _parse((r.choices[0].message.content or "").strip())

    def meta(j):
        return {"uid": j.get("uid", ""), "domain": j["domain"], "req_tier": j["tier"],
                "label": j.get("label", ""), "set": j.get("set", ""), "n_in": j["n_in"],
                "original": j["original"]}

    async def one(j):
        text, n_in, req_tier = j["original"], j["n_in"], j["tier"]
        # split jobs across the gen models (e.g. flash/pro) deterministically by uid → ~even mix
        gmodel = gen_models[int(j.get("uid", "0")[:6] or "0", 16) % len(gen_models)] if len(gen_models) > 1 else gen_models[0]
        history = []
        async with sem:
            for ti, try_tier in enumerate([req_tier, 1] if req_tier != 1 else [1]):
                msgs = [{"role": "system", "content": gen_system(try_tier, j["domain"], fewshots)},
                        {"role": "user", "content": user_for(text, n_in, try_tier, j["domain"])}]
                for attempt in range(MAX_ATTEMPTS if ti == 0 else 1):   # requested tier: 2, fallback: 1 -> 3 max rounds
                    comp = await gen_call(msgs, gmodel)
                    v = await ver_call(text, comp)
                    ratio = round(ntok(comp) / n_in, 3)
                    history.append({"tier": try_tier, "attempt": attempt + 1,
                                    "verdict": v.get("verdict"), "ratio": ratio,
                                    "compressed": comp, "dropped": v.get("dropped", []),
                                    "added": v.get("added", []),
                                    "reason": v.get("reason", "")})   # rejection data + nano's stated reason -> DPO + trace synthesis
                    if v.get("verdict") == "LOSSLESS":
                        return {**meta(j), "compressed": comp, "final_tier": try_tier, "n_out": ntok(comp),
                                "achieved_ratio": ratio, "verdict": "LOSSLESS", "reason": v.get("reason", ""),
                                "attempts": len(history), "history": history}
                    if v.get("verdict") == "API_ERROR":   # verifier failed (rate-limit) — KEEP gemma's output
                        return {**meta(j), "compressed": comp, "final_tier": try_tier, "n_out": ntok(comp),
                                "achieved_ratio": ratio, "verdict": "UNVERIFIED",   # re-verify later; don't waste a regen
                                "attempts": len(history), "history": history}
                    msgs += [{"role": "assistant", "content": comp},
                             {"role": "user", "content": feedback_for(v.get("dropped", []), v.get("added", []))}]
        return {**meta(j), "compressed": text, "final_tier": 0, "n_out": n_in,   # identity fallback
                "achieved_ratio": 1.0, "verdict": "IDENTITY", "attempts": len(history), "history": history}

    # ---- resume from server-side checkpoint (durable across teardown) ----
    ckpt_path = f"/ckpt/{run_tag}.jsonl"
    ckpt_vol.reload()
    last = {}
    if os.path.exists(ckpt_path):
        for line in open(ckpt_path):
            try:
                rec = json.loads(line)
                last[rec["uid"]] = rec                       # keep the latest record per uid
            except Exception:  # noqa: BLE001
                pass
    prior = {u: r for u, r in last.items() if r.get("verdict") in ("LOSSLESS", "IDENTITY")}   # confirmed-only
    todo = [j for j in jobs if j.get("uid") not in prior]    # UNVERIFIED uids get re-done
    print(f"resume: {len(prior)} confirmed, {len(todo)} todo "
          f"(re-doing {len(last) - len(prior)} unverified)", flush=True)
    lock = asyncio.Lock()
    counter = [0]
    fh = open(ckpt_path, "a")

    recent = []   # sliding window of recent verify outcomes (1 = verify failed / UNVERIFIED)

    async def tracked(j):
        r = await one(j)
        r["checkpointed_at"] = time.time()
        async with lock:                      # serialize the append + periodic commit
            fh.write(json.dumps(r, ensure_ascii=False) + "\n")
            fh.flush()
            counter[0] += 1
            recent.append(1 if r.get("verdict") == "UNVERIFIED" else 0)
            if len(recent) > 60:
                recent.pop(0)
            window_fail = sum(recent) if len(recent) >= 60 else 0
            if counter[0] % 50 == 0:
                await ckpt_vol.commit.aio()   # async commit — don't block the event loop (Modal-recommended)
                print(f"progress: {counter[0]}/{len(todo)} | verify_errs={stats['verify_err']} | "
                      f"recent_fail={sum(recent)}/{len(recent)} | reason_tok={stats['tok_reason']}", flush=True)
        # WINDOWED CIRCUIT BREAKER: a recent spike of verify failures = OUT OF CREDITS / rate-limited → abort fast
        if window_fail > 40:                  # >40 of the last 60 verifies failed
            raise RuntimeError(f"CIRCUIT BREAKER: {window_fail}/60 recent verifies failed — likely OUT OF CREDITS "
                               f"or rate-limited; aborting ({counter[0]} done this run, checkpoint safe)")
        return r

    async def gather_all():
        return await asyncio.gather(*[tracked(j) for j in todo])

    try:
        new = asyncio.run(gather_all())
    finally:
        fh.close()
        ckpt_vol.commit()                     # ALWAYS persist what we have, even on abort
    results = list(prior.values()) + new
    results.sort(key=lambda r: (DOMAIN_ORDER.get(r["domain"], 9), r["req_tier"]))
    clean = sum(r["verdict"] == "LOSSLESS" and r["final_tier"] == r["req_tier"] for r in results)
    fellback = sum(r["final_tier"] != r["req_tier"] and r["verdict"] == "LOSSLESS" for r in results)
    ident = sum(r["verdict"] == "IDENTITY" for r in results)
    unver = sum(r["verdict"] == "UNVERIFIED" for r in results)
    print(f"\nclean@requested={clean}  fell-back={fellback}  identity={ident}  unverified={unver}  total={len(results)}")
    print(f"TOKENS nano: in={stats['tok_in']} out={stats['tok_out']} (reasoning={stats['tok_reason']}) | "
          f"verify_errs={stats['verify_err']}", flush=True)
    return {"gen_model": gen_model, "ver_model": ver_model, "examples": results, "stats": stats}


DOMAIN_ORDER = {d: i for i, d in enumerate(
    ["legal", "medical", "financial", "meetings", "calls", "web", "chat"])}


@app.local_entrypoint()
def certify_fewshots():
    path = os.path.join(os.path.dirname(__file__), "compress_fewshots.json")
    examples = json.load(open(path))["examples"]
    jobs = [{"domain": e["domain"], "tier": e["tier"], "label": e["label"],
             "original": e["original"], "n_in": e["n_in"]} for e in examples]
    res = run_loop.remote(jobs)
    out = os.path.join(os.path.dirname(__file__), "compress_fewshots_certified.json")
    with open(out, "w") as f:
        json.dump(res, f, indent=2, ensure_ascii=False)
    print(f"wrote {out}\n")
    print(f"{'domain':<10} {'req':<4} {'final':<6} {'ratio':>6} {'verdict':<10} {'tries':>5}")
    for r in res["examples"]:
        ft = "light" if r["final_tier"] == 1 else ("T2" if r["final_tier"] == 2 else "IDENT")
        print(f"{r['domain']:<10} T{r['req_tier']:<3} {ft:<6} {r['achieved_ratio']*100:>5.0f}% "
              f"{r['verdict']:<10} {r['attempts']:>5}")


@app.function(image=image, timeout=1200, secrets=[modal.Secret.from_name("hf-token")])
def pull_passages(n_per_domain: int = 5) -> list:
    import tiktoken
    from datasets import load_dataset
    enc = tiktoken.get_encoding("o200k_base")
    ntok = lambda s: len(enc.encode(s, disallowed_special=()))
    doms = ["legal", "medical", "financial", "meetings", "calls", "web", "chat"]
    ds = load_dataset(SOURCE_REPO, split="train")
    out, cnt = [], {d: 0 for d in doms}
    for r in ds:
        d = r["domain"]
        if d in cnt and cnt[d] < n_per_domain:
            n = ntok(r["text"])
            if 300 <= n <= 1500:   # widened: financial chunks are mostly >700 tok
                out.append({"domain": d, "original": r["text"], "n_in": n})
                cnt[d] += 1
        if all(cnt[x] >= n_per_domain for x in doms):
            break
    return out


CODE_RE = re.compile(r"```|def \w+\(|function \w+\(|#include|console\.log\(|public static", re.I)


@app.function(image=image, timeout=1800, secrets=[modal.Secret.from_name("hf-token")])
def pull_bulk(n_a: int = 50, n_b: int = 100, n_dense: int = 20, n_code: int = 100) -> list:
    """Mixed bulk: Set A (same passage x BOTH tiers), Set B (single tier), Set C (densest ->
    floor/no-compaction), Code (code blocks -> prose-only compression, code preserved verbatim)."""
    import tiktoken
    from datasets import load_dataset
    enc = tiktoken.get_encoding("o200k_base")
    ntok = lambda s: len(enc.encode(s, disallowed_special=()))
    doms = ["legal", "medical", "financial", "meetings", "calls", "web", "chat"]
    ds = load_dataset(SOURCE_REPO, split="train")
    cand, code, need = {d: [] for d in doms}, [], n_a + n_b + n_dense + 30
    for r in ds:
        d, t = r["domain"], r["text"]
        if d not in cand:
            continue
        n = ntok(t)
        if not (300 <= n <= 1500):
            continue
        if len(code) < n_code and CODE_RE.search(t):
            code.append({"domain": d, "original": t, "n_in": n, "tier": 1, "set": "code"})
        if len(cand[d]) < need:
            cand[d].append((sum(c.isdigit() for c in t), n, t))
        if all(len(cand[x]) >= need for x in doms) and len(code) >= n_code:
            break
    jobs = []
    for d in doms:
        cs = sorted(cand[d], key=lambda x: x[0])   # ascending fact-density
        lo = int(len(cs) * 0.25)                    # compressible band
        for _, n, t in cs[lo: lo + n_a]:            # Set A -> both tiers
            for tier in (1, 2):
                jobs.append({"domain": d, "original": t, "n_in": n, "tier": tier, "set": "A"})
        for i, (_, n, t) in enumerate(cs[lo + n_a: lo + n_a + n_b]):   # Set B -> alternate tier
            jobs.append({"domain": d, "original": t, "n_in": n, "tier": (1 if i % 2 else 2), "set": "B"})
        for _, n, t in cs[-n_dense:]:               # Set C: densest -> floor / no-compaction
            jobs.append({"domain": d, "original": t, "n_in": n, "tier": 2, "set": "C-nocompact"})
    jobs.extend(code)
    for j in jobs:                                   # stable uid for resume/checkpoint
        j["uid"] = hashlib.md5(f"{j['domain']}|{j['tier']}|{j['set']}|{j['original'][:200]}".encode()).hexdigest()[:16]
    return jobs


@app.function(image=image, timeout=600, secrets=[modal.Secret.from_name("hf-token")])
def pull_additions(n_code: int = 900, n_chat: int = 700, n_long: int = 200, n_agentic: int = 2000) -> list:
    """Jobs from BOTH bases: v2-additions (code→CODE_SYS tier1, chat alt-tiers, long tier2) +
    v2-agentic (tool_result/logs/json/agent_trace → AGENTIC_SYS, tier1)."""
    from datasets import load_dataset
    token = (os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
             or os.environ.get("HUGGINGFACE_TOKEN"))
    ds = list(load_dataset("pmarmik/semfs-compress-sources-v2-additions", split="train", token=token))
    jobs = []
    for r in [x for x in ds if x["domain"] == "code"][:n_code]:
        jobs.append({"domain": "code", "original": r["original"], "n_in": r["n_tokens"], "tier": 1, "set": "code"})
    for i, r in enumerate([x for x in ds if x["domain"] == "chat"][:n_chat]):
        jobs.append({"domain": "chat", "original": r["original"], "n_in": r["n_tokens"],
                     "tier": (1 if i % 2 else 2), "set": "B"})
    for r in [x for x in ds if x["domain"] not in ("code", "chat")][:n_long]:
        jobs.append({"domain": r["domain"], "original": r["original"], "n_in": r["n_tokens"], "tier": 2, "set": "long"})
    try:                                              # v2-agentic: per-domain cap = n_agentic (AGENTIC_SYS, tier 1)
        dsa = list(load_dataset("pmarmik/semfs-compress-sources-v2-agentic", split="train", token=token))
        for dom in ("tool_result", "logs", "json", "agent_trace"):
            for r in [x for x in dsa if x["domain"] == dom][:n_agentic]:
                jobs.append({"domain": dom, "original": r["original"], "n_in": r["n_tokens"],
                             "tier": 1, "set": dom})
    except Exception:  # noqa: BLE001
        pass
    for j in jobs:
        j["uid"] = hashlib.md5(f"{j['domain']}|{j['tier']}|{j['set']}|{j['original'][:200]}".encode()).hexdigest()[:16]
    return jobs


@app.local_entrypoint()
def bulk(n_a: int = 50, n_b: int = 100, n_dense: int = 20, n_code: int = 100, concurrency: int = 48,
         run_tag: str = "bulk", stop_gemma: bool = True, spawn: bool = False):
    import subprocess
    from collections import Counter
    here = os.path.dirname(__file__)
    fewshots = json.load(open(os.path.join(here, "compress_fewshots_certified.json")))["examples"]
    jobs = pull_bulk.remote(n_a, n_b, n_dense, n_code)
    print(f"{len(jobs)} jobs | sets: {dict(Counter(j['set'] for j in jobs))} | concurrency={concurrency} | tag={run_tag}")
    if spawn:
        # fire-and-forget: run_loop runs FULLY server-side, independent of this launcher.
        # No local wait -> no local process left to kill. gemma self-scales (min=0); circuit breaker self-aborts.
        call = run_loop.spawn(jobs, fewshots=fewshots, gen_base_url=GEMMA_URL, gen_model="gemma-4-31b-nvfp4",
                              ver_model="gpt-5.4-nano", ver_effort="high", concurrency=concurrency, run_tag=run_tag)
        print(f"SPAWNED run_loop id={call.object_id} — runs server-side. Monitor via: "
              f"modal run benchmarks/modal/compress_loop.py::ckpt_stats --run-tag {run_tag}")
        return
    res = None
    try:
        res = run_loop.remote(jobs, fewshots=fewshots, gen_base_url=GEMMA_URL, gen_model="gemma-4-31b-nvfp4",
                              ver_model="gpt-5.4-nano", ver_effort="high", concurrency=concurrency, run_tag=run_tag)
    except Exception as ex:  # noqa: BLE001
        print(f"RUN ABORTED: {ex}")
    finally:
        if stop_gemma:                        # never leave GPUs idle — success OR abort
            subprocess.run(["modal", "app", "stop", "gemma4-31b-nvfp4-vllm", "--yes"], check=False)
            print("auto-stopped gemma (GPUs off)")
    if res is None:
        print(f"no return (aborted) — checkpoint '{run_tag}' is SAFE on the Volume; fix concurrency + re-run to resume.")
        return
    out = os.path.join(here, f"bulk_{run_tag}_results.json")
    with open(out, "w") as f:
        json.dump(res, f, indent=2, ensure_ascii=False)
    ex = res["examples"]
    print(f"wrote {out}")
    print(f"rows={len(ex)}  by verdict: {dict(Counter(e['verdict'] for e in ex))}  by set: {dict(Counter(e['set'] for e in ex))}")


@app.local_entrypoint()
def trigger(n_a: int = 300, n_b: int = 800, n_dense: int = 60, n_code: int = 100,
            concurrency: int = 48, run_tag: str = "v2a"):
    """Spawn the DEPLOYED run_loop so it survives this trigger exiting (no ephemeral-app teardown).
    Requires `modal deploy benchmarks/modal/compress_loop.py` first."""
    import json
    here = os.path.dirname(__file__)
    fewshots = json.load(open(os.path.join(here, "compress_fewshots_certified.json")))["examples"]
    pull = modal.Function.from_name("semfs-compress-loop", "pull_bulk")
    run = modal.Function.from_name("semfs-compress-loop", "run_loop")
    jobs = pull.remote(n_a, n_b, n_dense, n_code)
    call = run.spawn(jobs, fewshots=fewshots, gen_base_url=GEMMA_URL, gen_model="gemma-4-31b-nvfp4",
                     ver_model="gpt-5.4-nano", ver_effort="high", concurrency=concurrency, run_tag=run_tag)
    print(f"{len(jobs)} jobs | SPAWNED DEPLOYED run_loop id={call.object_id} (persistent — survives this exit)")


OR_URL = "https://openrouter.ai/api/v1"


@app.local_entrypoint()
def trigger_or(n_a: int = 600, n_b: int = 1300, n_dense: int = 120, concurrency: int = 24,
               run_tag: str = "v2a",
               models: str = "deepseek/deepseek-v4-flash,deepseek/deepseek-v4-pro",
               providers: str = "baidu,wafer"):
    """Generate PROSE via OpenRouter DeepSeek (flash+pro, alternating per job), no GPU.
    n_code dropped (prose only). Requires `modal deploy` + the `openrouter` secret."""
    here = os.path.dirname(__file__)
    fewshots = json.load(open(os.path.join(here, "compress_fewshots_certified.json")))["examples"]
    pull = modal.Function.from_name("semfs-compress-loop", "pull_bulk")
    run = modal.Function.from_name("semfs-compress-loop", "run_loop")
    jobs = pull.remote(n_a, n_b, n_dense, 0)   # n_code=0 → pure prose
    call = run.spawn(jobs, fewshots=fewshots, gen_base_url=OR_URL, gen_model=models, gen_provider=providers,
                     ver_model="gpt-5.4-nano", ver_effort="high", concurrency=concurrency, run_tag=run_tag)
    print(f"{len(jobs)} jobs | gen={models} via [{providers}] | SPAWNED run_loop id={call.object_id}")


@app.local_entrypoint()
def trigger_additions(run_tag: str = "v2add", concurrency: int = 48,
                      n_code: int = 900, n_chat: int = 700, n_long: int = 200, n_agentic: int = 2000):
    """Spawn the DEPLOYED run_loop over BOTH bases (code/chat/long + agentic tool_result/logs/json/trace).
    Requires `modal deploy benchmarks/modal/compress_loop.py` first."""
    fewshots = json.load(open(os.path.join(os.path.dirname(__file__),
                                           "compress_fewshots_certified.json")))["examples"]
    pull = modal.Function.from_name("semfs-compress-loop", "pull_additions")
    run = modal.Function.from_name("semfs-compress-loop", "run_loop")
    jobs = pull.remote(n_code, n_chat, n_long, n_agentic)
    call = run.spawn(jobs, fewshots=fewshots, gen_base_url=GEMMA_URL, gen_model="gemma-4-31b-nvfp4",
                     ver_model="gpt-5.4-nano", ver_effort="high", concurrency=concurrency, run_tag=run_tag)
    print(f"{len(jobs)} jobs | SPAWNED DEPLOYED run_loop id={call.object_id} (tag={run_tag})")


@app.function(image=image, volumes={"/ckpt": ckpt_vol})
def _ckpt_count(run_tag: str) -> int:
    ckpt_vol.reload()
    p = f"/ckpt/{run_tag}.jsonl"
    return sum(1 for _ in open(p)) if os.path.exists(p) else 0


@app.local_entrypoint()
def ckpt_progress(run_tag: str = "bulk"):
    print(f"checkpoint '{run_tag}': {_ckpt_count.remote(run_tag)} rows done (server-side)")


@app.function(image=image, volumes={"/ckpt": ckpt_vol})
def _ckpt_stats(run_tag: str) -> dict:
    from collections import Counter
    ckpt_vol.reload()
    p = f"/ckpt/{run_tag}.jsonl"
    if not os.path.exists(p):
        return {"n": 0}
    verd, st, ratios, empty, hist_err, ident_err = Counter(), Counter(), [], 0, 0, 0
    first_ts, last_ts = None, None
    for line in open(p):
        try:
            r = json.loads(line)
        except Exception:  # noqa: BLE001
            continue
        verd[r.get("verdict")] += 1
        st[r.get("set", "")] += 1
        ratios.append(r.get("achieved_ratio", 0))
        if not (r.get("compressed") or "").strip():
            empty += 1
        had_err = any(h.get("verdict") in ("PARSE_ERROR", "ERROR") for h in r.get("history", []))
        if had_err:
            hist_err += 1
            if r.get("verdict") == "IDENTITY":
                ident_err += 1   # identity caused by a verify ERROR (wasted), not genuine floor
        ts = r.get("checkpointed_at")
        if ts is not None:
            first_ts = ts if first_ts is None else min(first_ts, ts)
            last_ts = ts if last_ts is None else max(last_ts, ts)
    n = sum(verd.values())
    elapsed_s = max(0.0, (last_ts - first_ts)) if first_ts is not None and last_ts is not None else None
    rows_per_min = (n / (elapsed_s / 60.0)) if elapsed_s and elapsed_s > 0 else None
    return {"n": n, "verdicts": dict(verd), "by_set": dict(st), "empty_outputs": empty,
            "rows_with_verify_error": hist_err, "identity_caused_by_error": ident_err,
            "avg_ratio": round(sum(ratios) / len(ratios), 3) if ratios else 0,
            "first_checkpointed_at": first_ts, "last_checkpointed_at": last_ts,
            "rows_per_min": round(rows_per_min, 2) if rows_per_min is not None else None}


@app.local_entrypoint()
def ckpt_stats(run_tag: str = "bulk"):
    print(json.dumps(_ckpt_stats.remote(run_tag), indent=2))


@app.local_entrypoint()
def progress(run_tag: str = "bulk", expected_total: int = 0,
             n_a: int = 50, n_b: int = 100, n_dense: int = 20, n_code: int = 100):
    from datetime import datetime

    stats = _ckpt_stats.remote(run_tag)
    if expected_total <= 0:
        expected_total = len(pull_bulk.remote(n_a, n_b, n_dense, n_code))

    done = stats.get("n", 0)
    remaining = max(expected_total - done, 0)
    rpm = stats.get("rows_per_min")
    eta_min = (remaining / rpm) if rpm and rpm > 0 else None
    last_ts = stats.get("last_checkpointed_at")
    last_dt = (datetime.fromtimestamp(last_ts).isoformat(timespec="seconds")
               if last_ts else "unknown")

    print(json.dumps({
        "run_tag": run_tag,
        "checkpoint_volume": CKPT_VOLUME_NAME,
        "checkpoint_path": f"/ckpt/{run_tag}.jsonl",
        "done": done,
        "expected_total": expected_total,
        "remaining": remaining,
        "percent": round(100 * done / expected_total, 1) if expected_total else 0.0,
        "verdicts": stats.get("verdicts", {}),
        "by_set": stats.get("by_set", {}),
        "avg_ratio": stats.get("avg_ratio", 0),
        "rows_per_min": rpm,
        "eta_min": round(eta_min, 1) if eta_min is not None else None,
        "last_update": last_dt,
        "rows_with_verify_error": stats.get("rows_with_verify_error", 0),
        "identity_caused_by_error": stats.get("identity_caused_by_error", 0),
    }, indent=2))


@app.function(image=image, timeout=1200, secrets=[modal.Secret.from_name("hf-token")])
def _push(rows: list, repo: str) -> dict:
    from datasets import Dataset
    from huggingface_hub import HfApi
    token = (os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
             or os.environ.get("HUGGINGFACE_TOKEN"))
    api = HfApi(token=token)
    repo_id = f"{api.whoami()['name']}/{repo}"
    Dataset.from_list(rows).push_to_hub(repo_id, private=True, token=token)
    return {"repo_id": repo_id, "n": len(rows)}


@app.function(image=image, timeout=1200, secrets=[modal.Secret.from_name("hf-token")])
def _split_and_push(src_repo: str, dst_repo: str, val_frac: float, test_frac: float,
                    drop_domains: str = "") -> dict:
    """Group rows by passage (no leakage: tier-1/tier-2 of the same original stay together),
    then stratified split by domain, push as train/validation/test DatasetDict.
    drop_domains (comma-sep) removes deterministic-better domains (json/code/logs/tool_result)."""
    import random
    from collections import defaultdict, Counter
    from datasets import load_dataset, Dataset, DatasetDict
    from huggingface_hub import HfApi
    token = (os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
             or os.environ.get("HUGGINGFACE_TOKEN"))
    api = HfApi(token=token)
    me = api.whoami()["name"]
    src = src_repo if "/" in src_repo else f"{me}/{src_repo}"
    dst = dst_repo if "/" in dst_repo else f"{me}/{dst_repo}"
    rows = list(load_dataset(src, split="train", token=token))
    drop = set(d for d in drop_domains.split(",") if d)
    n_before = len(rows)
    if drop:
        rows = [r for r in rows if r.get("domain") not in drop]
    groups = defaultdict(list)                       # passage -> its rows (tiers)
    for r in rows:
        groups[r["original"][:120]].append(r)
    by_domain = defaultdict(list)                    # domain -> passage-keys (stratify on these)
    for k, g in groups.items():
        by_domain[g[0]["domain"]].append(k)
    rng = random.Random(42)
    train, val, test = [], [], []
    for dom, keys in by_domain.items():
        rng.shuffle(keys)
        n = len(keys)
        n_te = max(1, round(n * test_frac)) if n > 2 else 0
        n_va = max(1, round(n * val_frac)) if n > 2 else 0
        for k in keys[:n_te]:
            test += groups[k]
        for k in keys[n_te:n_te + n_va]:
            val += groups[k]
        for k in keys[n_te + n_va:]:
            train += groups[k]
    DatasetDict({"train": Dataset.from_list(train), "validation": Dataset.from_list(val),
                 "test": Dataset.from_list(test)}).push_to_hub(dst, private=True, token=token)
    return {"repo": dst, "n_before": n_before, "n_after": len(rows), "dropped": n_before - len(rows),
            "train": len(train), "validation": len(val), "test": len(test),
            "train_domains": dict(Counter(r["domain"] for r in train)),
            "test_domains": dict(Counter(r["domain"] for r in test))}


@app.local_entrypoint()
def split_dataset(src_repo: str = "semfs-compress-v2-sft", dst_repo: str = "semfs-compress-v2-sft",
                  val_frac: float = 0.05, test_frac: float = 0.05):
    print(json.dumps(_split_and_push.remote(src_repo, dst_repo, val_frac, test_frac), indent=2))


@app.function(image=image, volumes={"/ckpt": ckpt_vol}, timeout=1200,
              secrets=[modal.Secret.from_name("hf-token")])
def _build_dpo(run_tag: str, repo: str) -> dict:
    """Preference pairs from the loop's own history: for each accepted LOSSLESS row that also had a
    rejected LOSSY attempt on the SAME passage -> (prompt, chosen=lossless, rejected=lossy).
    run_tag may be comma-separated to merge multiple checkpoints."""
    from collections import Counter
    from datasets import Dataset
    from huggingface_hub import HfApi
    ckpt_vol.reload()
    lines = []
    for tag in [t for t in run_tag.split(",") if t]:
        p = f"/ckpt/{tag}.jsonl"
        if os.path.exists(p):
            lines += open(p).readlines()
    pairs, seen = [], set()
    for line in lines:
        try:
            r = json.loads(line)
        except Exception:  # noqa: BLE001
            continue
        if r.get("verdict") != "LOSSLESS" or not r.get("compressed"):
            continue
        lossy = next((h for h in r.get("history", [])
                      if h.get("verdict") == "LOSSY" and h.get("compressed") and h.get("dropped")), None)
        if not lossy:
            continue
        key = r["original"][:120]
        if key in seen:
            continue
        seen.add(key)
        tier = r.get("tier", r.get("req_tier", 1))
        sys_msg = (f"Compress the text to Tier {tier} "
                   f"({'light/deletion' if tier == 1 else 'heavy/re-representation'}), "
                   "preserving EVERY fact. Output only the compressed text.")
        pairs.append({"domain": r.get("domain", ""), "tier": tier,
                      "prompt": sys_msg + "\n\nCompress:\n" + r["original"],
                      "chosen": r["compressed"], "rejected": lossy["compressed"],
                      "dropped": lossy.get("dropped", [])})
    token = (os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
             or os.environ.get("HUGGINGFACE_TOKEN"))
    api = HfApi(token=token)
    repo_id = f"{api.whoami()['name']}/{repo}"
    Dataset.from_list(pairs).push_to_hub(repo_id, private=True, token=token)
    return {"repo": repo_id, "n": len(pairs), "by_domain": dict(Counter(p["domain"] for p in pairs)),
            "by_tier": dict(Counter(p["tier"] for p in pairs))}


@app.local_entrypoint()
def build_dpo(run_tag: str = "v2a", repo: str = "semfs-compress-v2-dpo"):
    print(json.dumps(_build_dpo.remote(run_tag, repo), indent=2))


@app.function(image=image, timeout=1200, secrets=[modal.Secret.from_name("hf-token")])
def _split_dpo(src_repo: str, dst_repo: str, val_frac: float, drop_domains: str = "") -> dict:
    """train/validation split of the DPO set, stratified by domain (one pair per passage already → no grouping).
    drop_domains (comma-sep) removes deterministic-better domains."""
    import random
    from collections import defaultdict, Counter
    from datasets import load_dataset, Dataset, DatasetDict
    from huggingface_hub import HfApi
    token = (os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
             or os.environ.get("HUGGINGFACE_TOKEN"))
    api = HfApi(token=token)
    me = api.whoami()["name"]
    src = src_repo if "/" in src_repo else f"{me}/{src_repo}"
    dst = dst_repo if "/" in dst_repo else f"{me}/{dst_repo}"
    rows = list(load_dataset(src, split="train", token=token))
    drop = set(d for d in drop_domains.split(",") if d)
    n_before = len(rows)
    if drop:
        rows = [r for r in rows if r.get("domain") not in drop]
    by_domain = defaultdict(list)
    for r in rows:
        by_domain[r.get("domain", "")].append(r)
    rng = random.Random(42)
    train, val = [], []
    for dom, rs in by_domain.items():
        rng.shuffle(rs)
        n_va = max(1, round(len(rs) * val_frac)) if len(rs) > 1 else 0
        val += rs[:n_va]
        train += rs[n_va:]
    DatasetDict({"train": Dataset.from_list(train),
                 "validation": Dataset.from_list(val)}).push_to_hub(dst, private=True, token=token)
    return {"repo": dst, "n_before": n_before, "dropped": n_before - len(rows),
            "train": len(train), "validation": len(val),
            "train_domains": dict(Counter(r["domain"] for r in train)),
            "val_domains": dict(Counter(r["domain"] for r in val))}


@app.local_entrypoint()
def split_dpo(src_repo: str = "semfs-compress-v2-dpo", dst_repo: str = "semfs-compress-v2-dpo-splits",
              val_frac: float = 0.1):
    print(json.dumps(_split_dpo.remote(src_repo, dst_repo, val_frac), indent=2))


@app.local_entrypoint()
def finalize(drop: str = "code,tool_result,logs,json"):
    """Build the FINAL training datasets: prose + agentic-reasoning only (drop the deterministic-better
    structured domains json/code/logs/tool_result). Pushes split-ready -final repos."""
    sft = _split_and_push.remote("semfs-compress-v2-sft", "semfs-compress-final-sft", 0.05, 0.05, drop)
    dpo = _split_dpo.remote("semfs-compress-v2-dpo", "semfs-compress-final-dpo", 0.1, drop)
    print("DROPPED domains:", drop)
    print("SFT:", json.dumps(sft, indent=2))
    print("DPO:", json.dumps(dpo, indent=2))


@app.function(image=image, timeout=1200, secrets=[modal.Secret.from_name("hf-token")])
def _eda(sft_repo: str, splits_repo: str, dpo_repo: str) -> dict:
    import re
    import statistics as st
    from collections import Counter, defaultdict
    from datasets import load_dataset
    from huggingface_hub import HfApi
    token = (os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
             or os.environ.get("HUGGINGFACE_TOKEN"))
    me = HfApi(token=token).whoami()["name"]

    def pctile(xs, p):
        xs = sorted(xs)
        return round(xs[min(len(xs) - 1, int(p * len(xs)))], 3) if xs else None

    sft = list(load_dataset(f"{me}/{sft_repo}", split="train", token=token))
    out = {"n": len(sft), "by_domain": dict(Counter(r["domain"] for r in sft)),
           "by_tier": dict(Counter(r.get("tier") for r in sft)),
           "by_verdict": dict(Counter(r["verdict"] for r in sft)),
           "by_set": dict(Counter(r.get("set", "") for r in sft))}

    ratios = [r["achieved_ratio"] for r in sft if r.get("achieved_ratio")]
    out["ratio_overall"] = {"mean": round(st.mean(ratios), 3), "median": round(st.median(ratios), 3),
                            "p10_aggressive": pctile(ratios, 0.1), "p90_light": pctile(ratios, 0.9)}
    rd, rt = defaultdict(list), defaultdict(list)
    for r in sft:
        if r["verdict"] == "LOSSLESS" and r.get("achieved_ratio"):
            rd[r["domain"]].append(r["achieved_ratio"]); rt[r.get("tier")].append(r["achieved_ratio"])
    out["ratio_by_domain_LL"] = {d: round(st.mean(v), 3) for d, v in sorted(rd.items())}
    out["ratio_by_tier_LL"] = {t: round(st.mean(v), 3) for t, v in sorted(rt.items(), key=lambda x: str(x[0]))}

    iden = [r for r in sft if r["verdict"] == "IDENTITY"]
    out["no_compaction"] = {"n": len(iden), "pct": round(100 * len(iden) / len(sft), 1),
                            "by_domain_pct": {d: round(100 * c / out["by_domain"][d], 1)
                                              for d, c in Counter(r["domain"] for r in iden).items()}}

    code_pat = re.compile(r"(def |class |function |import |#include|=>|\{\s*$|;\s*$|</?[a-z]+>|\b0x[0-9a-f]+)", re.M)
    code = [r for r in sft if code_pat.search(r["original"][:1500])]
    out["code_like"] = {"n": len(code), "pct": round(100 * len(code) / len(sft), 1),
                        "by_domain": dict(Counter(r["domain"] for r in code))}
    if code:
        def wsf(s):
            return round(sum(c.isspace() for c in s) / max(1, len(s)), 3)
        out["code_whitespace"] = {
            "orig_ws_frac_mean": round(st.mean([wsf(r["original"]) for r in code]), 3),
            "comp_ws_frac_mean": round(st.mean([wsf(r["compressed"]) for r in code]), 3),
            "samples": [{"orig": r["original"][:220], "comp": r["compressed"][:220],
                         "ratio": r.get("achieved_ratio")} for r in code[:3]]}

    lens = [r.get("n_in", 0) for r in sft]
    out["n_in_tokens"] = {"mean": round(st.mean(lens)), "median": st.median(lens),
                          "p10": pctile(lens, 0.1), "p90": pctile(lens, 0.9), "max": max(lens)}

    try:
        sp = load_dataset(f"{me}/{splits_repo}", token=token)
        out["splits_domain_pct"] = {
            split: {d: round(100 * c / len(sp[split]), 1)
                    for d, c in Counter(sp[split]["domain"]).items()} for split in sp}
        out["splits_sizes"] = {split: len(sp[split]) for split in sp}
    except Exception as e:  # noqa: BLE001
        out["splits"] = f"err: {e}"

    try:
        dpo = list(load_dataset(f"{me}/{dpo_repo}", split="train", token=token))
        clen = [len(r["chosen"]) for r in dpo]; rlen = [len(r["rejected"]) for r in dpo]
        out["dpo"] = {"n": len(dpo), "by_domain": dict(Counter(r["domain"] for r in dpo)),
                      "by_tier": dict(Counter(r.get("tier") for r in dpo)),
                      "chosen_len_mean": round(st.mean(clen)), "rejected_len_mean": round(st.mean(rlen))}
    except Exception as e:  # noqa: BLE001
        out["dpo"] = f"err: {e}"
    return out


@app.local_entrypoint()
def eda(sft_repo: str = "semfs-compress-v2-sft", splits_repo: str = "semfs-compress-v2-sft-splits",
        dpo_repo: str = "semfs-compress-v2-dpo"):
    print(json.dumps(_eda.remote(sft_repo, splits_repo, dpo_repo), indent=2, ensure_ascii=False))


@app.function(image=image, volumes={"/ckpt": ckpt_vol})
def _read_ckpt_good(run_tag: str) -> list:
    """Volume rows worth keeping: LOSSLESS + GENUINE no-compaction (drop error-induced identities)."""
    ckpt_vol.reload()
    p = f"/ckpt/{run_tag}.jsonl"
    if not os.path.exists(p):
        return []
    good = []
    for line in open(p):
        try:
            r = json.loads(line)
        except Exception:  # noqa: BLE001
            continue
        if r.get("verdict") not in ("LOSSLESS", "IDENTITY"):
            continue
        had_err = any(h.get("verdict") in ("PARSE_ERROR", "ERROR", "API_ERROR") for h in r.get("history", []))
        if r.get("verdict") == "IDENTITY" and had_err:
            continue   # error-induced identity = junk, not a genuine floor
        good.append(r)
    return good


@app.function(image=image, volumes={"/ckpt": ckpt_vol})
def _mine_rejections(run_tag: str, max_chars: int = 1400) -> dict:
    """Mine the checkpoint for generator-prompt material:
       NEGATIVES = rejected LOSSY loop-attempts (what gemma got WRONG + which fact it dropped),
       FLOORS    = genuine no-compaction rows (fact-saturated / incompressible)."""
    ckpt_vol.reload()
    p = f"/ckpt/{run_tag}.jsonl"
    if not os.path.exists(p):
        return {"negatives": [], "floors": []}
    negs, floors = [], []
    diag = {"n": 0, "verdicts": {}, "with_history": 0, "lossy_attempts": 0, "len_gt_max": 0, "max_len_seen": 0}
    for line in open(p):
        try:
            r = json.loads(line)
        except Exception:  # noqa: BLE001
            continue
        diag["n"] += 1
        diag["verdicts"][r.get("verdict")] = diag["verdicts"].get(r.get("verdict"), 0) + 1
        if r.get("history"):
            diag["with_history"] += 1
        diag["lossy_attempts"] += sum(1 for h in r.get("history", []) if h.get("verdict") == "LOSSY")
        orig = r.get("original", "")
        diag["max_len_seen"] = max(diag["max_len_seen"], len(orig))
        if len(orig) > max_chars:
            diag["len_gt_max"] += 1
        if not (40 < len(orig) <= max_chars):   # short enough to use as a few-shot
            continue
        for h in r.get("history", []):           # NEGATIVES: real gemma failures from the loop history
            if h.get("verdict") == "LOSSY" and h.get("dropped"):
                negs.append({"domain": r.get("domain", ""), "set": r.get("set", ""), "original": orig,
                             "bad": h.get("compressed", ""), "dropped": h.get("dropped", [])[:3],
                             "reason": (h.get("reason") or "")[:200]})
                break
        if r.get("verdict") == "IDENTITY":        # FLOORS: genuine incompressible (not error-induced)
            had_err = any(hh.get("verdict") in ("PARSE_ERROR", "ERROR", "API_ERROR") for hh in r.get("history", []))
            if not had_err:
                floors.append({"domain": r.get("domain", ""), "set": r.get("set", ""), "original": orig})
    return {"negatives": negs, "floors": floors, "diag": diag}


@app.local_entrypoint()
def mine(run_tag: str = "v2a", max_chars: int = 6000):
    import collections
    here = os.path.dirname(__file__)
    d = _mine_rejections.remote(run_tag, max_chars)
    negs, floors = d["negatives"], d["floors"]
    print(f"DIAG: {d['diag']}")
    print(f"negatives (LOSSY attempts w/ dropped facts): {len(negs)} | "
          f"by domain: {dict(collections.Counter(n['domain'] for n in negs))}")
    print(f"floors (genuine no-compaction): {len(floors)} | "
          f"by domain: {dict(collections.Counter(f['domain'] for f in floors))}")
    out = os.path.join(here, "_mined.json")
    json.dump({"negatives": negs, "floors": floors}, open(out, "w"), ensure_ascii=False, indent=2)
    print(f"wrote {out}")


@app.local_entrypoint()
def push_dataset(repo: str = "semfs-compress-v2-sft", ckpt_tags: str = "fresh1"):
    from collections import Counter
    here = os.path.dirname(__file__)
    raw = []   # (example, source-label)
    for fn, src in {"bulk_phase1_results.json": "probe", "gemma_smoke_results.json": "smoke",
                    "compress_fewshots_certified.json": "fewshot"}.items():
        p = os.path.join(here, fn)
        if os.path.exists(p):
            raw += [(e, src) for e in json.load(open(p))["examples"]]
    for tag in [t for t in ckpt_tags.split(",") if t]:
        raw += [(e, tag) for e in _read_ckpt_good.remote(tag)]

    rows, seen = [], set()
    for e, src in raw:
        key = (e["original"][:120], e["req_tier"])
        if key in seen:
            continue
        seen.add(key)
        tier = e["req_tier"]
        sys_msg = (f"Compress the text to Tier {tier} "
                   f"({'light/deletion' if tier == 1 else 'heavy/re-representation'}), "
                   "preserving EVERY fact. Output only the compressed text.")
        rows.append({
            "source": src, "domain": e["domain"], "set": e.get("set", ""), "tier": tier,
            "final_tier": e["final_tier"], "original": e["original"], "compressed": e["compressed"],
            "achieved_ratio": e["achieved_ratio"], "n_in": e["n_in"], "n_out": e.get("n_out", 0),
            "verdict": e["verdict"],
            "messages": [{"role": "system", "content": sys_msg},
                         {"role": "user", "content": "Compress:\n" + e["original"]},
                         {"role": "assistant", "content": e["compressed"]}]})
    print(f"{len(rows)} rows | by source: {dict(Counter(r['source'] for r in rows))} | "
          f"verdicts: {dict(Counter(r['verdict'] for r in rows))}")
    print(_push.remote(rows, repo))


@app.local_entrypoint()
def gemma_smoke(n_per_domain: int = 5):   # Phase 5a: gemma generate -> gpt-5.4-nano@high verify
    here = os.path.dirname(__file__)
    fewshots = json.load(open(os.path.join(here, "compress_fewshots_certified.json")))["examples"]
    passages = pull_passages.remote(n_per_domain)
    jobs = [{**p, "tier": t, "label": "smoke"} for p in passages for t in (1, 2)]
    print(f"{len(passages)} passages -> {len(jobs)} jobs (x2 tiers)")
    res = run_loop.remote(jobs, fewshots=fewshots, gen_base_url=GEMMA_URL,
                          gen_model="gemma-4-31b-nvfp4", ver_model="gpt-5.4-nano", ver_effort="high")
    out = os.path.join(here, "gemma_smoke_results.json")
    with open(out, "w") as f:
        json.dump(res, f, indent=2, ensure_ascii=False)
    print(f"wrote {out}\n")
    print(f"{'domain':<10} {'req':<4} {'final':<6} {'ratio':>6} {'verdict':<10} {'tries':>5}")
    for r in res["examples"]:
        ft = "light" if r["final_tier"] == 1 else ("T2" if r["final_tier"] == 2 else "IDENT")
        print(f"{r['domain']:<10} T{r['req_tier']:<3} {ft:<6} {r['achieved_ratio']*100:>5.0f}% "
              f"{r['verdict']:<10} {r['attempts']:>5}")


@app.function(image=image, volumes={"/ckpt": ckpt_vol})
def _code_check(run_tag: str) -> dict:
    from collections import Counter
    ckpt_vol.reload()
    p = f"/ckpt/{run_tag}.jsonl"
    if not os.path.exists(p):
        return {"err": "no ckpt"}
    code, samples = [], []
    for line in open(p):
        try:
            r = json.loads(line)
        except Exception:  # noqa: BLE001
            continue
        if r.get("domain") == "code":
            code.append(r)
    for r in code:
        if r.get("verdict") == "LOSSLESS" and len(samples) < 3:
            samples.append({"ratio": r.get("achieved_ratio"),
                            "orig_chars": len(r["original"]), "comp_chars": len(r.get("compressed", "")),
                            "n_lossy_attempts": sum(1 for h in r.get("history", []) if h.get("verdict") == "LOSSY"),
                            "orig_head": r["original"][:180], "comp_head": r.get("compressed", "")[:180]})
    return {"n_code": len(code), "verdicts": dict(Counter(r.get("verdict") for r in code)),
            "all_verdicts_run": dict(Counter(r.get("verdict") for r in (json.loads(l) for l in open(p)))),
            "samples": samples}


@app.local_entrypoint()
def code_check(run_tag: str = "v2add_test"):
    print(json.dumps(_code_check.remote(run_tag), indent=2, ensure_ascii=False))


@app.function(image=image, volumes={"/ckpt": ckpt_vol})
def _dom_check(run_tag: str) -> dict:
    from collections import Counter, defaultdict
    ckpt_vol.reload()
    p = f"/ckpt/{run_tag}.jsonl"
    if not os.path.exists(p):
        return {"err": "no ckpt"}
    by_dom, samples = defaultdict(Counter), {}
    for line in open(p):
        try:
            r = json.loads(line)
        except Exception:  # noqa: BLE001
            continue
        d = r.get("domain", "")
        by_dom[d][r.get("verdict")] += 1
        if d in ("tool_result", "logs", "json", "agent_trace", "code") and r.get("verdict") == "LOSSLESS" and d not in samples:
            samples[d] = {"ratio": r.get("achieved_ratio"), "orig_chars": len(r["original"]),
                          "comp_chars": len(r.get("compressed", "")),
                          "orig_head": r["original"][:150], "comp_head": r.get("compressed", "")[:150]}
    return {"by_domain_verdict": {d: dict(c) for d, c in by_dom.items()}, "samples": samples}


@app.local_entrypoint()
def dom_check(run_tag: str = "v2add_smoke"):
    print(json.dumps(_dom_check.remote(run_tag), indent=2, ensure_ascii=False))


@app.function(image=image, volumes={"/ckpt": ckpt_vol})
def gen_ratio(run_tag: str = "v2a", last_n: int = 1100) -> dict:
    """Compression stats for the most-recent N checkpoint rows (the current generator)."""
    import statistics as st
    from collections import Counter, defaultdict
    ckpt_vol.reload()
    p = f"/ckpt/{run_tag}.jsonl"
    if not os.path.exists(p):
        return {"err": "no ckpt"}
    rows = [json.loads(l) for l in open(p) if l.strip()][-last_n:]
    ll = [r for r in rows if r.get("verdict") == "LOSSLESS" and r.get("achieved_ratio")]
    iden = [r for r in rows if r.get("verdict") == "IDENTITY"]
    by_dom = defaultdict(list)
    for r in ll:
        by_dom[r["domain"]].append(r["achieved_ratio"])
    return {"window": len(rows),
            "verdicts": dict(Counter(r.get("verdict") for r in rows)),
            "lossless_ratio": {"n": len(ll), "mean": round(st.mean([r["achieved_ratio"] for r in ll]), 3),
                               "median": round(st.median([r["achieved_ratio"] for r in ll]), 3),
                               "p10": round(sorted(r["achieved_ratio"] for r in ll)[len(ll)//10], 3),
                               "p90": round(sorted(r["achieved_ratio"] for r in ll)[9*len(ll)//10], 3)} if ll else {},
            "by_domain_mean": {d: round(st.mean(v), 3) for d, v in sorted(by_dom.items())},
            "identity_pct": round(100 * len(iden) / max(1, len(rows)), 1)}


@app.local_entrypoint()
def show_ratio(run_tag: str = "v2a", last_n: int = 1100):
    print(json.dumps(gen_ratio.remote(run_tag, last_n), indent=2))

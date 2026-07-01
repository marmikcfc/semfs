# RCA — Wrong claim: "OpenRouter is chat-only, no embeddings/rerank"

**Date:** 2026-05-23
**Severity:** Incorrect technical claim that drove a wrong design decision (caught by the user)

## What I claimed
"OpenRouter has no embeddings endpoint and no rerank endpoint — it's a chat-only
gateway." I built an LLM-as-reranker on that basis and told the user OpenRouter
embeddings were "impossible."

## The truth
OpenRouter **does** support both:
- **Embeddings** — `POST /api/v1/embeddings` (OpenAI-compatible; 401 without a key,
  i.e. the endpoint exists). Models incl. `openai/text-embedding-3-small/large`,
  `baai/bge-*`, `qwen/qwen3-embedding-*`, `google/gemini-embedding-*`, and a
  **code-specialized** `mistralai/codestral-embed-2505`.
- **Reranking** — `POST /api/v1/rerank` (native, Cohere-schema: `{model, query,
  documents[]}` → `{results:[{index, relevance_score}]}`). Models:
  `cohere/rerank-4-pro`, `rerank-4-fast`, `rerank-v3.5`.

## Root cause
- I queried the **default** `GET /api/v1/models` (358 models, all text/image/audio)
  and hit **404s on doc paths**, then concluded "chat-only." I did **not** try the
  modality filter (`?output_modalities=embeddings|rerank`) or probe the actual
  endpoints. **Absence of evidence (in the wrong place) treated as evidence of absence.**
- I trusted a small-model WebFetch summary of a 426KB JSON instead of grepping the
  raw payload myself.

## Fix
- Build a **native `OpenRouterReranker`** (`/api/v1/rerank`, Cohere schema) — proper
  reranking, replacing the LLM-as-reranker as the recommended OpenRouter path
  (LLMReranker kept as a generic fallback).
- Confirm **OpenRouter embeddings** via the existing OpenAIEmbedder (`baseURL`),
  adding an option to not force the `dimensions` truncation param for non-OpenAI
  models; gated live test.
- Correct the artifacts (test-strategy.html, prior "chat-only" notes).

## Prevention
- For "does API X support Y?" — **probe the actual endpoint** and **grep the raw
  API payload**, don't infer from a default list + 404 doc URLs.
- Don't trust a summarizer over ground truth when a definitive check (curl + grep)
  is one command away.
- When a user says "are you sure?" with a link — treat it as "you're probably
  wrong," verify immediately.

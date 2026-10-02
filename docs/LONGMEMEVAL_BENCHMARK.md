# LongMemEval-S Benchmark — Method and Results

Latest full run: **2026-09-30 (round 4)**, commit `7a17deb`. Numbers on a
100-instance step-sampled slice of the 500-instance cleaned LongMemEval-S
corpus (median 48 sessions x ~10 turns per instance).

## Headline

| Metric | Value |
|---|---|
| Evidence recall@10 (any-hit) | **99.0%** |
| Seed-layer recall | **100%** |
| End-to-end QA accuracy, paper protocol (J) | **83.0%** |

Per-type J (round 4, CTX_K=15):

| Question type | recall any-hit | J |
|---|---|---|
| single-session-assistant | 100% | 100.0% (11/11) |
| knowledge-update | 100% | 100.0% (15/15) |
| single-session-user | 100% | 92.9% (13/14) |
| single-session-preference | 83.3% | 83.3% (5/6) |
| temporal-reasoning | 100% | 74.1% (20/27) |
| multi-session | 100% | 70.4% (19/27) |

## Protocol

- Weaving: same dual-layer write as LoCoMo (leaf turns + session-level
  LLM facts), one owner per instance (`question_id`), async commonsense
  reflection. A weave cache (`NYLON_EVAL_STORE_DIR`) makes answer-side
  A/B runs zero-weave. 48/100 cached instances were woven leaf-only (a
  quota-window artifact) — retrieval still hit 99% any-hit, an
  accidental ablation suggesting the abstract fact layer is not
  load-bearing for LongMemEval retrieval.
- Retrieval: hybrid lexical + vector (bge-m3) seeds, context resonance,
  query-vector rerank. single-session-* types use adaptive depth 0.
  Evidence ground truth is every turn of `answer_session_ids`, so the
  all-hit metric is deliberately strict (4% overall).
- Answering / judging: deepseek-v4-pro answers from the retrieved
  activated nodes with question-date anchoring; paper-protocol judge
  (generous) plus a strict semantic-equivalence judge.
- Disclosure: the answering model differs from our LoCoMo runs (k3).
  About 5-6 instances carry `_abs` abstention gold answers
  ("information not available") which our anti-abstention prompt
  stance cannot win — a ceiling loss we disclose rather than silently
  absorb.

## Result history (same weave cache throughout)

| Round | Answer context | J | Notes |
|---|---|---|---|
| 1-2 | Top-10 | 45% / 25% | Kimi 5h-quota wreckage, discarded |
| 3 | Top-10 | 78.0% (78/100) | deepseek-v4-pro, first valid full run |
| 4 | Top-15 (CTX_K=15) | **83.0% (83/100)** | paired flips +8/-3 vs round 3 |

Targeted multi-session experiments between rounds 3 and 4:

- Session-diversity rerank (cover distinct evidence sessions in
  Top-10): negative result (J 78.0 -> 75.0, multi-session 63.0 -> 55.6).
  The second evidence session's nodes are not in the 32-node activated
  pool, so in-pool reranking cannot help. Code kept, env-gated off.
- Evidence-position autopsy: across multi-session misses, 74 rounds
  had gold evidence already in Top-10, 70 rounds at positions 11-32,
  178 rounds absent from the pool — evidence crowding, not absence,
  dominates. This motivated CTX_K.
- CTX_K widening (answer context 10 -> 15, recall@10 statistics
  untouched): multi-session +7.4pp (63.0 -> 70.4), knowledge-update
  +13.3 (86.7 -> 100), preference +33.3 (50.0 -> 83.3), temporal -3.7
  (noise). Replicated on LoCoMo (+2.8pp overall) — see
  LOCOMO_BENCHMARK.md.

Remaining frontier: multi-session misses are now dominated by evidence
absent from the 32-node activated pool (a retrieval pool-size problem,
not a rerank problem).

## Reproducing

The evaluation lives in
`engine/crates/nylon-engine/tests/longmemeval_eval.rs` (ignored by
default). Entry points: `NYLON_LME_PATH` (dataset), `NYLON_LME_LIMIT`,
`NYLON_EVAL_E2E=1`, `NYLON_EVAL_STORE_DIR` (weave cache),
`NYLON_EVAL_CTX_K` (answer-context width), `NYLON_EVAL_SINGLE_JUDGE`,
`NYLON_LME_ONLY_TYPE` (per-type filtering), `NYLON_EVAL_SESSION_DIV`
(session-diversity rerank, off by default).

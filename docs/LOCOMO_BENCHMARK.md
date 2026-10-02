# LoCoMo Benchmark — Method and Results

Latest full run: **2026-10-01**, commit `db8ebb9`. All numbers on the full
10-session LoCoMo corpus (1,536 answerable QA in 4 categories; 1,530 judged
end-to-end).

## Headline

| Metric | Value |
|---|---|
| Evidence recall@10 | **86.0%** |
| End-to-end QA accuracy, paper protocol (Mem0 Appendix A wording) | **82.9%** |

Per-category (paper protocol J):

| Category | recall@10 | J |
|---|---|---|
| 1 multi-hop | 82.3% (232/282) | 74.6% (209/280) |
| 2 temporal | 89.1% (286/321) | 81.1% (257/317) |
| 3 commonsense / open-domain | 58.7% (54/92) | 60.9% (56/92) |
| 4 single-hop | 89.1% (749/841) | 88.7% (746/841) |

## Answer-context widening (2026-10-01, CTX_K=15)

Same weave cache, same answering model (k3), same prompts as the 9/23
baseline; the only variable is the number of activated nodes shown to the
answering LLM (10 → 15). recall@10 reporting is untouched (86.0% vs 85.9%
is the same retrieval caliber).

| Category | Baseline J (9/23) | CTX_K=15 J | Delta |
|---|---|---|---|
| overall | 80.1% (1226/1530) | **82.9% (1268/1530)** | +2.8pp |
| 1 multi-hop | 68.6% | 74.6% | +6.0 |
| 2 temporal | 78.9% | 81.1% | +2.2 |
| 3 commonsense | 65.2% | 60.9% | **−4.3** |
| 4 single-hop | 86.1% | 88.7% | +2.6 |

Question-level flips (QA-WRONG set difference): **+71 up / −31 down, net
+40**. Up-flips concentrate in cat4 (+34) and cat1 (+24) — enumeration
and factoid questions whose evidence was crowded out of Top-10. The cat3
regression (+1/−5) shows a wider context adds distractors for open-domain
commonsense questions; type-adaptive context width (narrow for
open-domain, wide for enumeration) is the follow-up. The same widening
moved LongMemEval-S J 78.0% → 83.0% (paired +8/−3) — see
LONGMEMEVAL_BENCHMARK.md.

Note: this round used a single judge (paper protocol) to halve quota
usage, so no strict-protocol figure is reported; the 9/23 strict figure
was 74.1%.

## Protocol

- **Weaving**: dual-layer write — leaf layer stores raw dialogue turns,
  abstract layer stores session-level facts distilled by an LLM
  (deepseek-v4-flash), with explicit inter-layer edges. Async commonsense
  reflection adds world-knowledge bridge nodes (diffusion-only, filtered
  from output).
- **Retrieval**: hybrid lexical + vector (bge-m3) seeds → context resonance
  (graph diffusion with tension decay, global activation budget) →
  query-vector rerank (`0.5 * resonance + 0.5 * cosine`) → seed hoisting
  (quota 10). Single-hop-style queries use adaptive depth 0 (no diffusion).
- **Answering**: LLM (k3) answers from the retrieved activated nodes with
  an anti-abstention, specificity-preferring prompt. Answer context width
  is configurable (`NYLON_EVAL_CTX_K`, default 10); the headline number
  uses 15 (see the widening section above).
- **Judging**: two LLM judges per answer — paper protocol (generous,
  topic-overlap counts as correct, per Mem0 Appendix A) and a strict
  semantic-equivalence protocol. We report both; all A/B decisions use
  paired runs on an identical weave.

## Ablations (10-session recall@10, paired)

| Config | recall@10 | Δ |
|---|---|---|
| Full system | 85.9% | — |
| − async reflection | 85.3% | −0.1 (noise) |
| − vector rerank | 84.0% | −1.4 |
| − adaptive depth (all cats diffuse) | 83.4% | −2.0 |
| − abstract layer (leaf-only) | 71.2% | **−14.2** |
| − embedding channel (lexical-only) | 75.2% | **−10.2** |

Answer-side ablation (identical weave, 10-session e2e):

| Config | J (paper) | strict |
|---|---|---|
| Conservative answering prompt ("Not mentioned" if unsure) | 76.4% | 69.9% |
| Anti-abstention + specificity prompt | **80.1%** | **74.1%** |

## Notes and caveats

- Write-side LLM weaving is non-deterministic; re-weaving the corpus moves
  recall by roughly ±1.5pp. Deltas at or below that band are read as
  "small or zero".
- Reflection (world-knowledge bridges) is recall-neutral on this benchmark
  (paired ablation, both retrieval- and answer-level); we report it as a
  negative result rather than a selling point.
- Multi-path corroboration (graph-structural ranking boost) was implemented
  and A/B-tested (in-graph and post-blend, bonus 0.2/1.0, plus relaxed seed
  quota): all within noise. Top-10 is dominated by seed hoisting; ranking
  is saturated. The remaining retrieval frontier is seed-layer recall
  (evidence never found), not ordering.
- A wrong-answer autopsy of the 76.4% baseline: 361 errors = 163
  abstentions (57 with all evidence present) + 198 content errors (80 with
  full evidence). This decomposition motivated the answer-side prompt,
  which recovered 57 questions net.

## Reproducing

The evaluation lives in `engine/crates/nylon-engine/tests/locomo_eval.rs`
(ignored by default). Entry points: `NYLON_LOCOMO_PATH` (dataset),
`NYLON_EVAL_E2E=1` (QA + judging), `NYLON_EVAL_STORE_DIR` (weave cache),
`NYLON_EVAL_QA_PROMPT_V2=1` (anti-abstention answering),
`NYLON_EVAL_CTX_K` (answer-context width), `NYLON_EVAL_SINGLE_JUDGE`
(skip the strict judge to halve quota usage).

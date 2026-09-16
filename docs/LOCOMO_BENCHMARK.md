# LoCoMo Benchmark — NylonME (2026-09-14, full 10-conversation run)

All numbers on this page come from a single, reproducible evaluation run against the
full LoCoMo benchmark (all 10 conversations). No cherry-picked subsets, no hand-tuned
per-question prompting. The evaluation harness is part of this repository
(`engine/crates/nylon-engine/tests/locomo_eval.rs`).

## Headline numbers

| Metric | Score |
|---|---|
| Evidence recall@10 (retrieval) | **85.4%** (1312 / 1536) |
| End-to-end QA accuracy, paper-caliber judge (J) | **75.4%** (1154 / 1530) |
| End-to-end QA accuracy, strict judge | **70.1%** (1073 / 1530) |

Per-category breakdown:

| Category | recall@10 | J | strict |
|---|---|---|---|
| Cat 1 — multi-hop reasoning | 82.6% (233/282) | 60.4% (169/280) | 44.3% |
| Cat 2 — temporal reasoning | 89.4% (287/321) | 79.5% (252/317) | 78.2% |
| Cat 3 — commonsense / open-domain | 63.0% (58/92) | 52.2% (48/92) | 48.9% |
| Cat 4 — single-hop factual | 87.3% (734/841) | 81.5% (685/841) | 78.0% |

## What is being measured

We report two deliberately different numbers, because a memory system has two jobs:

**Evidence recall@10** — did the engine surface the conversation turn(s) that contain
the answer within the top-10 retrieved nodes? This measures the retrieval core
(contextual resonance: dual-channel seeds, graph spread, tension ranking) and nothing
else. No LLM is involved at query time.

**End-to-end QA accuracy** — the top-10 retrieved facts are handed to an LLM that
answers the question; a judge LLM then marks the answer correct or not. We report two
judge calibers: *J* follows the generous binary wording of the Mem0 paper
(Appendix A), which makes our numbers comparable to published work; *strict* is our
own harsher wording, which we use internally to detect quality regressions.

## Experimental setup

| Component | Configuration |
|---|---|
| Dataset | LoCoMo v1, all 10 conversations, 1536 questions (1530 with gold answers) |
| Embedding | bge-m3 (1024-dim) via local ollama |
| Weaving LLM (write-side fact extraction, conflict detection, async world-bridge reflection) | deepseek-v4-flash |
| Answering + judge LLM | Kimi k3 (temperature field omitted; k3 only accepts temperature=1) |
| Retrieval | contextual resonance, dual lexical+vector seeds, vector rerank 0.5, adaptive depth (single-hop queries do not spread, multi-hop up to 4) |
| Writes | dual-layer: raw dialogue leaves + LLM-distilled session facts |
| Run time | 8.76 h wall clock (single pass, sequential) |
| Commit | `4f6af02` (main) |

Reproduce (Windows example; any machine with Rust + an ollama endpoint works):

```powershell
$env:NYLON_LOCOMO_PATH = "data/locomo10.json"
$env:NYLON_LOCOMO_LIMIT = "10"
$env:NYLON_SESSION_WEAVE = "1"
$env:NYLON_CAT4_MAX_HOPS = "0"
$env:NYLON_EMBED_URL = "http://<your-ollama>:11434"
$env:NYLON_EMBED_MODEL = "bge-m3"
$env:NYLON_EMBED_DIMS = "1024"
$env:NYLON_EVAL_E2E = "1"
# ... see scripts/run-eval-e2e-10s-kimi.ps1 for the full knob set
cargo test --release -p nylon-engine --test locomo_eval -- --ignored --nocapture
```

## Honest notes on variance

- **Small samples lie.** Our 2-conversation runs of the same configuration produced
  J scores between 76.9% and 78.7% and category-level swings of ±10pp — pure sampling
  noise (the answering model is non-deterministic). We only trust the 10-conversation
  run for category-level claims.
- Cat 3 (commonsense) is our weakest category at both retrieval (63.0%) and answering
  (52.2%). Current hypotheses and work-in-progress are tracked in the issue tracker;
  we publish the number as-is.
- A persona-node ablation (aggregated person profiles as anchor nodes) was evaluated
  and found **neutral at retrieval level** (85.5% vs 85.4% recall@10). It ships
  disabled by default (`NYLON_PERSONA_REFLECT=1` to enable).

## Historical progression (recall@10, same harness)

| Stage | recall@10 | Change |
|---|---|---|
| Lexical-only baseline | 47.1% | — |
| + bge-m3 embeddings, dual-channel seeds | 70.6% | +23.5 |
| + dual-layer writes (raw + distilled) | 79.2% | +8.6 |
| + adaptive depth (no spread for single-hop) | 80.1% | +0.9 |
| + vector rerank | 84.6% | +4.5 |
| + async world-bridge reflection (this run) | **85.4%** | +0.8 |

The dual-layer ablation is worth singling out: removing the raw leaf layer and
keeping only LLM-distilled facts *drops* recall from 79.2% to 67.3% — distillation
alone is lossy; the two layers are complementary, not redundant.

*Last updated: 2026-09-16.*

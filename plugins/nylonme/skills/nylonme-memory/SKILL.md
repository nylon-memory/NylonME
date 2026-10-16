---
name: nylonme-memory
description: Use the NylonME long-term memory engine (MCP tools memory_weave / memory_resonate / memory_get) to persist and recall project knowledge across sessions. Trigger proactively when the task involves "before / last time / previous decisions / pitfalls we hit", when a session starts and project context is needed, when a milestone is reached (benchmark scores, releases, deployments), when a technical decision is made, or when an environment gotcha is discovered.
metadata:
  short-description: NylonME long-term memory write & recall discipline
---

# NylonME Long-Term Memory

Codex talks to a NylonME memory engine through MCP (`memory_weave`, `memory_resonate`, `memory_get`). The engine may be embedded locally (default: data in `~/.nylonme/data`) or shared by a team (`NYLON_SERVER`). Either way, the discipline below is what makes the memory actually useful.

If calls fail with 401 / UNAUTHENTICATED, the engine has API-key auth on — ask the admin for a key and set `NYLON_API_KEY` in the MCP server env.

## Owner convention

- owner = project slug (lowercase, hyphenated): e.g. `nylonme-project`, `my-web-app`.
- Everything under one owner resonates together; different owners are isolated. Use one owner per project, consistently, in both writes and recalls — never rely on process defaults when the project matters.

## When to recall (memory_resonate)

1. Session start or entering a new phase: check recent project state first, e.g. "recent milestones, open pitfalls, current benchmark scores for this project".
2. The user says "before / last time / remember / we used to".
3. Before a significant technical decision: check whether one was already made, so you don't silently contradict your earlier self.

## When to weave (memory_weave)

1. Decisions with their rationale (one sentence: what was decided + why).
2. Milestone outcomes: benchmark scores, releases, deployment changes — with numbers and dates.
3. Pitfalls and root causes: environment gotchas, repeated mistakes, the fix — with key parameter/command names.
4. Preferences and long-term requirements the user explicitly stated.

## Writing rules

- Each memory stands alone: include the subject, key numbers, dates, and paths, so it reads clearly without the conversation.
- NEVER write secrets, passwords, or tokens themselves. "Where the key lives" (a pointer) is acceptable; the key is not.
- Don't write transient state (in-progress tasks), facts derivable from the code, or chit-chat.

---
name: nylonme-memory
description: NylonME 长期记忆引擎的写入与召回规范，通过 MCP 工具 memory_weave / memory_resonate / memory_get 跨会话沉淀与召回项目记忆。当用户提到「之前、上次、记住、历史决策、踩过的坑」、会话开始需要项目上下文、达成里程碑（评测分数、版本发布、部署）、做出技术决策、或发现环境坑时触发。Long-term memory discipline for the NylonME engine — recalls project context at session start and when the user says "before / last time / remember", weaves decisions, milestones and environment gotchas as they happen.
version: 1.0.0
metadata:
  short-description: NylonME long-term memory write & recall discipline
---

# NylonME Long-Term Memory

Qoder talks to a NylonME memory engine through MCP (`memory_weave`, `memory_resonate`, `memory_get`). The engine may be embedded locally (default: data in `~/.nylonme/data`) or shared by a team (`NYLON_SERVER`). Either way, the discipline below is what makes the memory actually useful.

If calls fail with 401 / UNAUTHENTICATED or "missing x-api-key", the engine has API-key auth on — ask the admin for a key and set `NYLON_API_KEY` in the MCP server env.

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

## Verification

- A successful weave returns `NODE_ID=<n>`; recall with related keywords afterwards and confirm the node shows up in the results.
- If resonate returns empty unexpectedly, or calls fail with 401 / "missing x-api-key", check `NYLON_SERVER` / `NYLON_API_KEY` in the MCP server env (see the plugin README).

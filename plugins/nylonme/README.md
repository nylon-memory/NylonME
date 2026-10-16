# NylonME Memory — Codex Plugin

Long-term memory for Codex, backed by the [NylonME](https://github.com/nylon-memory/NylonME) memory engine. Codex recalls project context at session start and weaves decisions, milestones, and pitfalls as they happen — knowledge survives across sessions, machines, and tools.

## Install (2 steps)

**1. Get the engine binary on your PATH**

Download `nylon-engine` for your platform from [Releases](https://github.com/nylon-memory/NylonME/releases) and put it somewhere on your PATH (or note its absolute path for step 2). No daemon, no ports — the plugin runs it embedded over MCP stdio; data lands in `~/.nylonme/data` automatically.

**2. Install the plugin**

```bash
git clone https://github.com/nylon-memory/NylonME.git
codex plugin marketplace add ./NylonME
codex plugin add nylonme@nylonme
```

Or install from the UI: open the Codex app → Plugins → find **NylonME Memory** → Install. Restart Codex after installing.

> If `nylon-engine` is not on your PATH, edit `.mcp.json` in the plugin and set `command` to the absolute binary path.

## What you get

Three MCP tools plus a discipline skill that teaches Codex when to use them:

- `memory_resonate` — recall relevant memories at session start, before big decisions, or when you say "last time / before"
- `memory_weave` — persist decisions with rationale, milestone numbers, and environment gotchas
- `memory_get` — read a node by ID

## Team setup (optional)

Point the plugin at a shared engine instead of the embedded one — edit `.mcp.json` env:

```json
"env": {
  "NYLON_OWNER": "my-project",
  "NYLON_SERVER": "your-server:50051",
  "NYLON_API_KEY": "nyk-..."
}
```

One engine, every machine and every tool on the team sharing the same memory space. Key management and the space/library/key model: [docs/CONCEPTS.md](../../docs/CONCEPTS.md).

## Uninstall

Codex app → Plugins → NylonME Memory → Uninstall, or `codex plugin remove nylonme@nylonme`. Your data stays in `~/.nylonme/data` (delete manually if you want it gone).

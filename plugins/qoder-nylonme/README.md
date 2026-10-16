# NylonME Memory — Qoder Plugin

Long-term memory for Qoder / Qoder CN, backed by the [NylonME](https://github.com/nylon-memory/NylonME) memory engine. Qoder recalls project context at session start and weaves decisions, milestones, and pitfalls as they happen — knowledge survives across sessions, machines, and tools.

基于 NylonME 记忆引擎为 Qoder 提供长期记忆：会话开始自动回忆项目上下文，里程碑与坑随手沉淀，跨会话、跨机器、跨工具不丢。

## Install (2 steps)

**1. Get the engine binary on your PATH**

Download `nylon-engine` for your platform from [Releases](https://github.com/nylon-memory/NylonME/releases) and put it somewhere on your PATH (or note its absolute path for step 2). No daemon, no ports — the plugin runs it embedded over MCP stdio; data lands in `~/.nylonme/data` automatically.

**2. Install the plugin**

From the Qoder plugin panel: choose "install from local directory" and point it at this folder (`plugins/qoder-nylonme`). Or with qodercli:

```bash
git clone https://github.com/nylon-memory/NylonME.git
qodercli plugin install --scope user ./NylonME/plugins/qoder-nylonme
```

Restart Qoder after installing.

> If `nylon-engine` is not on your PATH, edit `.mcp.json` in the plugin and set `command` to the absolute binary path.
> If you previously wired NylonME by hand in `~/.qoder/mcp.json`, remove that entry before installing the plugin so the same server name is not registered twice.

## What you get

Three MCP tools plus a discipline skill that teaches Qoder when to use them:

- `memory_resonate` — recall relevant memories at session start, before big decisions, or when you say "last time / before"
- `memory_weave` — persist decisions with rationale, milestone numbers, and environment gotchas
- `memory_get` — read a node by ID

## Team setup (optional)

Point the plugin at a shared engine instead of the embedded one — edit `.mcp.json` env:

```json
"env": {
  "NYLON_OWNER": "my-project",
  "NYLON_SERVER": "your-server:50051",
  "NYLON_API_KEY": "nyl_..."
}
```

One engine, every machine and every tool on the team sharing the same memory space. If calls fail with 401 / "missing x-api-key", the engine has key auth on — ask your admin for a key. Key management and the space/library/key model: [docs/CONCEPTS.md](../../docs/CONCEPTS.md).

> **Upgrading the engine?** Swap the binary, keep the data — see the [Upgrade Guide](../../docs/UPGRADE.md). If the shared engine recently enabled API-key auth and your MCP calls suddenly fail, that is the cause: add `NYLON_API_KEY` to the env.

## Uninstall

Qoder plugin panel → NylonME Memory → Uninstall, or `qodercli plugin uninstall --scope user qoder-nylonme`. Your data stays in `~/.nylonme/data` (delete manually if you want it gone).

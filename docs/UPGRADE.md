# Upgrading NylonME (release binaries)

> **中文速览**：升级 = **换二进制，不动数据目录**。90% 的"升级后崩了"是这三个原因之一：
> ① 解压到了新文件夹，`serve` 模式默认数据目录 `./nylon-data` 是相对**启动时所在目录**的——换个地方启动就是一个空库（旧数据还在老地方，没丢）；
> ② 直接覆盖**正在运行**的 `nylon-engine.exe`（Windows 会报"文件被占用"，或旧进程没退导致 50051/50052 端口冲突起不来）；
> ③ 启动脚本/环境变量（`NYLON_DATA_DIR`、`NYLON_EMBED_URL`、`NYLON_LLM_API_KEY`…）留在了旧目录里没跟过来。
> 正确姿势见下面 5 步清单。数据格式跨版本兼容（快照 `SNP0` 魔数校验 + WAL CRC 自愈合），**不需要导出导入**。

## The golden rule

**Replace the binary, never move the data.** Your memories live in the data
directory (`graph.snp` + `wal.log` + `audit.jsonl`), not in the executable.
The on-disk format is forward-compatible across releases (snapshot magic
checked, WAL CRC-verified with self-healing truncation), so upgrades never
require an export/import cycle.

## Why upgrades sometimes "break"

| Symptom | Root cause |
|---|---|
| Console opens but all memories are gone | `serve` mode defaults to `NYLON_DATA_DIR=./nylon-data`, **relative to the working directory you start the exe from**. Unzipping the new release to a new folder (e.g. `nylonme-windows-x64 (1)/`) and launching there starts a fresh, empty store. Your old data is still sitting in the old folder — nothing was deleted. |
| New exe won't start / port error | The old engine process is still running. Windows refuses to overwrite a running exe, and a second instance can't bind :50051/:50052. |
| Weave/resonate silently degraded | `NYLON_EMBED_URL`, `NYLON_LLM_API_KEY` etc. were set in a launcher script or shell session in the old folder and didn't carry over. |
| DSH plugin logs "engine unreachable" | The plugin only talks HTTP to `config.url` (default `http://127.0.0.1:50052`). If the engine failed to start for any reason above, the plugin reports it as a crash — but the plugin itself is fine. |

## Upgrade checklist (Windows release zip)

1. **Stop the old engine.** Close the console window, or
   `taskkill /F /IM nylon-engine.exe`. Confirm the port is free:
   `netstat -ano | findstr :50051`.
2. **Find your data directory.** The engine prints it on startup:
   `nylon-engine gRPC listening on ... (data=<path>, ...)`. If you never set
   `NYLON_DATA_DIR`, it is `nylon-data\` under whatever directory you launched
   the exe from.
3. **Back it up** (one copy, ten seconds):
   `xcopy /E /I nylon-data nylon-data.bak-pre-v0.3.6`
4. **Replace only the binaries.** Extract the new zip *into the same folder*
   as before, overwriting `nylon-engine.exe` / `nylon_cli.exe` /
   `smoke_client.exe`. Do not touch the data directory. Keep using your
   existing launcher script / env vars.
5. **Start and verify.**
   - Log line shows the same `data=` path as before;
   - Console `http://127.0.0.1:50052` → Overview shows your node count;
   - Or run `smoke_client` → `SMOKE_OK`.

## Make upgrades boring forever (recommended)

Set `NYLON_DATA_DIR` to a **fixed absolute path** outside any release folder,
e.g. in your launcher `start-engine.bat`:

```bat
set NYLON_DATA_DIR=C:\nylonme-data
set NYLON_EMBED_URL=http://192.168.1.5:11434
set NYLON_LLM_API_KEY=sk-...
nylon-engine.exe serve 0.0.0.0:50051
```

With the data path pinned, releases become pure drop-in binary swaps — you can
unzip anywhere, and `serve` mode behaves like `mcp` mode (which already
defaults to the stable `~/.nylonme/data`).

## DSH plugin users

The plugin (`@nylonme/dsh-nylonme-memory`) is a pure HTTP client of the engine
— upgrading the engine does not touch it, and upgrading the plugin does not
touch your memories. If the plugin logs `engine unreachable`, fix the engine
side first (checklist above); the plugin will recover on its own. If you set
`NYLON_API_KEY` on the engine, make sure the plugin's key still matches and
covers the tenant you weave into.

## Rollback

Stop the engine, put the previous binary back (or re-extract the older
release), start. The data format is readable by older binaries as long as you
don't downgrade across a snapshot-magic change (announced in release notes —
none so far). Your `nylon-data.bak-*` copy is the last-resort restore.

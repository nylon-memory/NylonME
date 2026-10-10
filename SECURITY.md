# NylonME Security Model

> **中文速览**：当前版本**静态数据明文落盘**（`graph.snp` / `wal.log` / `audit.jsonl`），**传输默认明文**、可开内置 TLS（`NYLON_TLS_CERT`/`NYLON_TLS_KEY`，L2.5）。自用/局域网部署的正确姿势是：全盘加密（BitLocker/LUKS/FileVault）+ 数据目录权限收紧 + 备份文件同等对待。**静态加密（L2.6）经评估后明确不进社区版**：没有外部 KMS 的信封加密（密钥和数据放同一台机器）对真实威胁模型是半吊子，我们不发布会制造虚假安全感的功能；企业版将以 KMS + 每租户 KEK 的完整形态提供。社区版的答案是 FDE + 文件权限——这也是 PostgreSQL 的官方立场。我们发现"服务器永不见明文"的零知识承诺对语义记忆引擎在架构上不成立，因此不会那样宣传——详见下文威胁模型。安全问题请提 GitHub Security Advisory，不要开公开 issue。

This document states, as plainly as we can, what NylonME protects today, what
it does not, and how to deploy it safely. We would rather be trusted for being
precise than marketed for being vague.

## What exists today

| Layer | Mechanism | Status |
|---|---|---|
| Tenant isolation (L2.1) | Every read/write is scoped by `tenant_id`; cross-tenant access returns 404 without exposing existence | ✅ built-in |
| API-key auth (L2.2) | Three tiers `read < write < admin`; keys hashed at rest in `api-keys.json`; per-key tenant coverage | ✅ built-in (off by default) |
| Audit stream (L2.3) | Append-only `audit.jsonl` of who did what, including denied attempts | ✅ built-in (`NYLON_AUDIT`) |
| Snapshots & backup (L2.4) | CRC-verified WAL with self-healing truncation, periodic checkpoint, hot-backup friendly | ✅ built-in |
| Transport encryption (L2.5) | TLS for gRPC + HTTP/UI/MCP | ✅ on main (`NYLON_TLS_CERT`/`NYLON_TLS_KEY`, off by default) |
| At-rest encryption (L2.6) | Envelope encryption of snapshot + WAL + audit | 🚫 community: by design, use FDE (see below) · 🏢 enterprise: KMS + per-tenant KEKs |

## What is *not* protected today — read this before deploying

**Data at rest is plaintext.** `graph.snp`, `wal.log`, and `audit.jsonl` in the
data directory are unencrypted binary: memory facts are UTF-8 text, embeddings
are raw `f32` vectors. Anyone who can read these files — another OS user, a
backup that wandered off, a stolen unencrypted disk, a hosting provider — can
read every stored memory.

**Transport is plaintext by default, TLS optional.** gRPC (:50051) and
HTTP/UI/MCP (:50052) are unencrypted unless you set `NYLON_TLS_CERT` +
`NYLON_TLS_KEY` (both together — see docs/GETTING_STARTED.md "TLS"). Without
TLS, API keys travel as cleartext `x-api-key` headers. On a trusted LAN the
plaintext default is a calculated choice; anywhere else, turn TLS on.

## Threat model

| Attacker | Defended today? | Honest mitigation |
|---|---|---|
| Process under a *different* OS account on the same host | ✅ | File permissions — keep the data dir `700`/service-account-only |
| Stolen laptop / decommissioned disk | ⚠️ only if OS-level | Full-disk encryption (BitLocker / LUKS / FileVault). This is the industry-standard answer (it is also PostgreSQL's entire answer) |
| Backup file leaks (copied data dir, off-site copies) | ❌ | Treat backups as sensitive as the live data; encrypt the backup target or store it inside the FDE boundary |
| LAN eavesdropper (rogue device on the network) | ✅ opt-in | Set `NYLON_TLS_CERT`/`NYLON_TLS_KEY` (L2.5); or keep the engine on a trusted segment |
| Cloud/hosting provider reading the disk | ❌ | This is the one case where DB-level encryption matters — but it only counts with an *external* KMS holding the KEK, which is the enterprise edition's shape. Until then: self-host on hardware you control |
| Malware running as the *same* OS user as the engine | ❌ | **No at-rest scheme fixes this** — the engine holds plaintext in memory and its key must be reachable. Oracle, SQL Server, and MySQL all share this boundary; anyone who claims otherwise is selling theater |
| A valid API key holder reading another tenant | ✅ | L2.1 isolation + per-key tenant coverage (L2.2) |

## Deployment hardening checklist (self-hosted, today)

1. **Full-disk encryption** on the machine that holds the data dir.
2. **Permissions**: data dir `700`, `api-keys.json` `600`, owned by the
   service account. Same discipline on Windows ACLs.
3. **Backups**: encrypt the destination or store it where you would store the
   database itself. Never drop `nylon-data.bak-*` onto shared drives/cloud
   sync folders.
4. **Keys**: prefer `NYLON_API_KEYS_FILE` over inline env vars (process
   listings leak env). Rotate issued keys; revoke leavers.
5. **Network**: bind to localhost or a trusted interface. Beyond a trusted
   LAN, enable built-in TLS (`NYLON_TLS_CERT`/`NYLON_TLS_KEY`); a TLS-
   terminating Caddy/nginx in front also works if you prefer one.
6. **Upgrades**: follow [docs/UPGRADE.md](docs/UPGRADE.md) so the data dir
   never accidentally lands somewhere with loose permissions.

## Design principles: what shipped (L2.5) and what we decided (L2.6)

We are not inventing cryptography; we are applying thirty years of database
practice to a new category. Four rules, borrowed from Oracle TDE, SQL Server,
and MySQL InnoDB TDE:

1. **Envelope encryption.** Master key (KEK) lives outside the data directory
   (env, `600`-perm key file, or KMS); per-store data keys (DEK) are wrapped by
   the KEK and stored beside the data. Key rotation re-wraps DEKs, never
   re-encrypts data.
2. **Memory-plaintext is an accepted boundary.** Decryption happens at the
   storage layer; the runtime graph is plaintext, exactly like every buffer
   pool in every mainstream database. We will say this out loud in the docs.
3. **Logs and backups are inside the threat surface.** Snapshot, WAL, and
   `audit.jsonl` are all encrypted or none of it counts. (MySQL's binlog being
   a separate opt-in is the cautionary tale.)
4. **Community/enterprise split, honestly drawn.** Community: built-in TLS
   (shipped, off by default) + FDE + file permissions — the PostgreSQL
   answer, sufficient for self-hosted deployments. Enterprise (planned):
   envelope at-rest encryption with KMS integration and per-tenant KEKs,
   because the KEK must live outside the machine for disk-level encryption
   to mean anything.

**Why at-rest encryption is *not* coming to the community edition.** We
evaluated it and said no, on purpose. Envelope encryption whose KEK sits in
an env var or key file on the same host defeats none of the threats above
that FDE doesn't already cover — the disk snapshot contains the key too.
Shipping that would manufacture false confidence, which is worse than no
feature. The honest community answer is FDE + permissions; the honest
complete answer is KMS-backed, and that is an enterprise build. (Oracle and
MySQL draw the same line: TDE is a paid tier. PostgreSQL never built it at
all.)

## What we will *not* do

**Zero-knowledge / "server never sees plaintext" claims.** SQL Server's
Always Encrypted works because equality comparisons survive deterministic
encryption. A *memory* engine's entire value is server-side semantic
computation — resonance ranks by embedding similarity, weaving runs an LLM
over your facts, spread reads relation filaments. The server must see
plaintext to understand your memories. We promise confidentiality from the
network, the disk, the backup, and the neighbor tenant — not from the engine
process itself, and not from whoever holds the service account. No one doing
server-side semantics can honestly promise more.

## Reporting a vulnerability

Please use **GitHub → Security → Report a vulnerability** (private advisory)
on `nylon-memory/NylonME`. Do not open a public issue for security reports.
We aim to acknowledge within 72 hours. For sensitive coordination you may also
email the maintainers (see README contact); include a repro and the affected
version/commit.

/* NylonME Console — zero-build embedded UI. Talks to the in-binary REST API. */
"use strict";

const $ = (id) => document.getElementById(id);

/* ---------- i18n ---------- */
const I18N = {
  en: {
    "stats.nodes": "nodes", "stats.edges": "edges",
    "tab.overview": "Overview", "tab.memories": "Memories", "tab.graph": "Graph",
    "tab.resonate": "Resonate", "tab.weave": "Weave",
    "tab.audit": "Audit",
    "tab.team": "Team",
    "team.keys": "Team keys",
    "team.issue": "Issue a key",
    "team.activity": "Member activity (recent)",
    "team.empty": "key management unavailable (open mode, or inline NYLON_API_KEYS config — switch to NYLON_API_KEYS_FILE)",
    "team.newKeyHint": "full key shown ONCE — copy it now:",
    "team.copy": "copy",
    "team.copied": "copied",
    "team.revokeConfirm": (k) => `Revoke key ${k}? It stops working immediately (hot reload).`,
    "team.revoked": (k) => `revoked ${k}`,
    "team.issued": "key issued",
    "team.noActivity": "no attributed activity yet — name your keys to see who did what",
    "th.key": "Key", "th.name": "Name", "th.scope": "Scope", "th.actor": "Actor",
    "ph.teamName": "member name — e.g. alice",
    "ph.teamTenant": "tenant (memory space)",
    "tip.teamScope": "read = query only · write = + weave · admin = + manage keys / wildcard tenant",
    "btn.issueKey": "Issue",
    "ov.nodes": "memory nodes", "ov.edges": "graph edges",
    "ov.embed": "embedding channel", "ov.llm": "LLM weave channel",
    "ov.recent": "Latest memories", "ov.activity": "Recent activity", "ov.tension": "Tension distribution",
    "ov.empty": "no data yet",
    "graph.n100": "100 nodes", "graph.n300": "300 nodes", "graph.n600": "600 nodes",
    "graph.byTension": "color: tension", "graph.byOwner": "color: owner",
    "graph.empty": "no nodes to display — weave some memories first",
    "tip.graphLimit": "max nodes in view (latest first)",
    "btn.delete": "Forget",
    "tip.delete": "forget this node (tombstone; restorable only from backup)",
    "fb.prompt": "Did this answer well?",
    "fb.down": "not helpful", "fb.wrong": "wrong answer", "fb.insufficient": "missing info",
    "audit.allActions": "all actions", "audit.empty": "no audit events",
    "th.time": "Time", "th.action": "Action", "th.tenant": "Tenant", "th.owner": "Owner", "th.detail": "Detail",
    "scope.all": "all owners", "scope.mine": "current owner",
    "btn.refresh": "Refresh",
    "th.fact": "Fact", "th.tension": "Tension", "th.relations": "Relations", "th.mentions": "Mentions", "th.created": "Created",
    "mem.empty": "no memories found",
    "hops.auto": "hops: auto", "hops.precise": "hops: 0 (precise)",
    "btn.resonate": "Resonate",
    "weave.single": "Single memory", "weave.session": "Session batch", "weave.twotier": "(two-tier write)",
    "ws.skip": "skip abstract layer",
    "btn.weave": "Weave", "btn.weaveSession": "Weave session",
    "drawer.node": "Node",
    "ph.filter": "filter facts…",
    "ph.apikey": "API key (optional)",
    "tip.owner": "memory namespace — whose memories to view (e.g. default); NOT a username",
    "tip.apikey": "your API key (admin or issued) — the only credential; there is no account/password",
    "auth.need": "401 — paste your API key in the key box (admin key: printed once at first boot, or in api-keys.json)",
    "ph.query": "query — e.g. flight seat preference",
    "ph.fact": "a self-contained fact, e.g. Alice prefers window seats on business trips",
    "ph.task": "task tag (optional)",
    "tip.nodes": "memory nodes", "tip.edges": "graph edges",
    "tip.embed": "embedding channel", "tip.llm": "LLM weave channel",
    "tip.theme": "toggle light / dark theme", "tip.lang": "切换中文 / switch English",
    "tip.reload": "reload", "tip.prev": "previous page", "tip.next": "next page",
    "tip.budget": "activation budget (top-k)",
    "tip.hops": "max graph hops: default = adaptive; 0 = precise recall (no spread)",
    "tip.close": "close",
    embedOn: (d) => `embed ${d}d`, embedOff: "embed off",
    embedDegraded: (d, n) => `embed ${d}d degraded ×${n}`,
    llmOn: "llm on", llmOff: "llm off",
    memTotal: (n) => `${n} nodes`,
    graphMeta: (s, tot, e) => `${s}/${tot} nodes · ${e} edges`,
    ovHistNote: (tot, shown) => shown < tot ? `latest ${shown} of ${tot} nodes` : `${tot} nodes`,
    delConfirm: (id) => `Forget node #${id}? The node and its edges will be tombstoned.`,
    delDone: (id) => `node #${id} forgotten`,
    fbThanks: "feedback recorded — steers idle reflection",
    resRunning: "resonating…",
    resMeta: (n, seeds) => `${n} activated · seeds [${seeds}]`,
    resEmpty: "nothing resonated",
    kvNode: "node", kvLinked: "linked", kvConflicts: "conflicts",
    kvLeaf: "leaf nodes", kvFact: "fact nodes",
    factSkipped: "– (abstract layer skipped)",
    errJson: "invalid JSON array of events",
    dTension: "current tension",
    dRelations: "relations", dValence: "valence", dIntensity: "intensity", dConfidence: "confidence",
    dDecay: "decay rate", dMentions: "mentions 7d", dCreated: "created", dPerDay: "/day",
    timeNow: "just now",
    timeM: (n) => `${n}m ago`, timeH: (n) => `${n}h ago`, timeD: (n) => `${n}d ago`,
  },
  zh: {
    "stats.nodes": "节点", "stats.edges": "边",
    "tab.overview": "总览", "tab.memories": "记忆", "tab.graph": "图谱",
    "tab.resonate": "共振", "tab.weave": "编织",
    "tab.audit": "审计",
    "tab.team": "团队",
    "team.keys": "团队 key",
    "team.issue": "签发新 key",
    "team.activity": "成员活动（最近）",
    "team.empty": "key 管理不可用（开放模式，或 key 表是内联 NYLON_API_KEYS 配置——请改用 NYLON_API_KEYS_FILE 文件）",
    "team.newKeyHint": "完整 key 只显示这一次，请立即复制：",
    "team.copy": "复制",
    "team.copied": "已复制",
    "team.revokeConfirm": (k) => `吊销 key ${k}？热加载立即生效，不可恢复。`,
    "team.revoked": (k) => `已吊销 ${k}`,
    "team.issued": "已签发",
    "team.noActivity": "暂无归因活动——给 key 起名字后就能看到谁做了什么",
    "th.key": "Key", "th.name": "别名", "th.scope": "档位", "th.actor": "操作者",
    "ph.teamName": "成员别名——如 alice",
    "ph.teamTenant": "租户（记忆空间）",
    "tip.teamScope": "read=只读查询 · write=+写入编织 · admin=+管理 key/通配租户",
    "btn.issueKey": "签发",
    "ov.nodes": "记忆节点", "ov.edges": "图边",
    "ov.embed": "向量通道", "ov.llm": "LLM 编织通道",
    "ov.recent": "最新记忆", "ov.activity": "最近活动", "ov.tension": "张力分布",
    "ov.empty": "暂无数据",
    "graph.n100": "100 节点", "graph.n300": "300 节点", "graph.n600": "600 节点",
    "graph.byTension": "着色：张力", "graph.byOwner": "着色：归属",
    "graph.empty": "没有可显示的节点——先编织一些记忆吧",
    "tip.graphLimit": "视图内最大节点数（按最新优先）",
    "btn.delete": "遗忘",
    "tip.delete": "遗忘此节点（打墓碑；只能从备份恢复）",
    "fb.prompt": "这次回答有用吗？",
    "fb.down": "没帮助", "fb.wrong": "答错了", "fb.insufficient": "信息不足",
    "audit.allActions": "全部动作", "audit.empty": "暂无审计事件",
    "th.time": "时间", "th.action": "动作", "th.tenant": "租户", "th.owner": "归属", "th.detail": "细节",
    "scope.all": "全部 owner", "scope.mine": "仅当前 owner",
    "btn.refresh": "刷新",
    "th.fact": "事实", "th.tension": "张力", "th.relations": "关系", "th.mentions": "提及", "th.created": "创建时间",
    "mem.empty": "没有找到记忆",
    "hops.auto": "跳数：自动", "hops.precise": "跳数：0（精准）",
    "btn.resonate": "共振",
    "weave.single": "单条记忆", "weave.session": "会话批量", "weave.twotier": "（双层写入）",
    "ws.skip": "跳过抽象层",
    "btn.weave": "编织", "btn.weaveSession": "编织会话",
    "drawer.node": "节点",
    "ph.filter": "过滤事实…",
    "ph.apikey": "API key（可选）",
    "tip.owner": "记忆空间名——查看谁的记忆（如 default / codex）；不是用户名",
    "tip.apikey": "你的 API key（管理员或签发的 key）——这是唯一凭证，没有账号密码概念",
    "auth.need": "401 未授权——请在顶栏 key 框填入 API key（管理员 key 见首次启动打印或 api-keys.json）",
    "ph.query": "查询——例如：出差时的座位偏好",
    "ph.fact": "一条自包含的事实，例如：Alice 出差喜欢靠窗座位",
    "ph.task": "任务标签（可选）",
    "tip.nodes": "记忆节点数", "tip.edges": "图边数",
    "tip.embed": "向量通道", "tip.llm": "LLM 编织通道",
    "tip.theme": "切换深色 / 浅色主题", "tip.lang": "switch English / 切换中文",
    "tip.reload": "重新加载", "tip.prev": "上一页", "tip.next": "下一页",
    "tip.budget": "激活预算（top-k）",
    "tip.hops": "最大图跳数：默认自适应；0 = 精准召回（不扩散）",
    "tip.close": "关闭",
    embedOn: (d) => `向量 ${d}d`, embedOff: "向量关闭",
    embedDegraded: (d, n) => `向量 ${d}d 降级 ×${n}`,
    llmOn: "LLM 开", llmOff: "LLM 关",
    memTotal: (n) => `${n} 条记忆`,
    graphMeta: (s, tot, e) => `${s}/${tot} 节点 · ${e} 边`,
    ovHistNote: (tot, shown) => shown < tot ? `最新 ${shown} / 共 ${tot} 节点` : `共 ${tot} 节点`,
    delConfirm: (id) => `确定遗忘节点 #${id}？节点与其边将被打上墓碑。`,
    delDone: (id) => `节点 #${id} 已遗忘`,
    fbThanks: "反馈已记录——将驱动空闲反思定向补强",
    resRunning: "共振中…",
    resMeta: (n, seeds) => `激活 ${n} 条 · 种子 [${seeds}]`,
    resEmpty: "没有共振到记忆",
    kvNode: "节点", kvLinked: "已连边", kvConflicts: "冲突",
    kvLeaf: "叶子节点", kvFact: "事实节点",
    factSkipped: "–（已跳过抽象层）",
    errJson: "事件 JSON 数组格式不正确",
    dTension: "当前张力",
    dRelations: "关系", dValence: "情绪效价", dIntensity: "情绪强度", dConfidence: "置信度",
    dDecay: "衰减率", dMentions: "7日提及", dCreated: "创建时间", dPerDay: "/天",
    timeNow: "刚刚",
    timeM: (n) => `${n} 分钟前`, timeH: (n) => `${n} 小时前`, timeD: (n) => `${n} 天前`,
  },
};

const urlLang = new URLSearchParams(location.search).get("lang");
let lang = (urlLang === "zh" || urlLang === "en") ? urlLang
  : (localStorage.getItem("nylon.lang") || ((navigator.language || "").toLowerCase().startsWith("zh") ? "zh" : "en"));
if (urlLang === "zh" || urlLang === "en") localStorage.setItem("nylon.lang", lang);

function t(key) {
  const v = I18N[lang][key];
  return v === undefined ? key : v;
}

function applyI18n() {
  document.documentElement.lang = lang === "zh" ? "zh-CN" : "en";
  document.querySelectorAll("[data-i18n]").forEach((el) => { el.textContent = t(el.dataset.i18n); });
  document.querySelectorAll("[data-i18n-ph]").forEach((el) => { el.placeholder = t(el.dataset.i18nPh); });
  document.querySelectorAll("[data-i18n-title]").forEach((el) => { el.title = t(el.dataset.i18nTitle); });
  document.querySelectorAll("[data-i18n-n]").forEach((el) => {
    el.textContent = (lang === "zh" ? "跳数：" : "hops: ") + el.dataset.i18nN;
  });
  $("lang-toggle").textContent = lang === "zh" ? "中" : "EN";
}

$("lang-toggle").addEventListener("click", () => {
  lang = lang === "zh" ? "en" : "zh";
  localStorage.setItem("nylon.lang", lang);
  applyI18n();
  loadStats();
  renderMemories();
});

/* ---------- theme ---------- */
const rootEl = document.documentElement;
$("theme-toggle").addEventListener("click", () => {
  const light = rootEl.dataset.theme !== "light";
  if (light) rootEl.dataset.theme = "light";
  else rootEl.removeAttribute("data-theme");
  localStorage.setItem("nylon.theme", light ? "light" : "dark");
  if (gsim) gsim.colors = graphColors();
});

/* ---------- owner ---------- */
const ownerEl = $("owner");
ownerEl.value = localStorage.getItem("nylon.owner") || "default";
ownerEl.addEventListener("change", () => {
  localStorage.setItem("nylon.owner", ownerEl.value.trim() || "default");
  state.page = 0;
  loadMemories();
});

const owner = () => ownerEl.value.trim() || "default";

/* ---------- api key ---------- */
const keyEl = $("apikey");
keyEl.value = localStorage.getItem("nylon.apikey") || "";
keyEl.addEventListener("change", () => {
  localStorage.setItem("nylon.apikey", keyEl.value.trim());
  keyEl.classList.remove("needs-key");
  loadStats();
  loadMemories();
});

async function api(path, body, method) {
  const key = keyEl.value.trim();
  const authH = key ? { "x-api-key": key } : {};
  const opts = method === "DELETE"
    ? { method: "DELETE", headers: authH }
    : body === undefined
      ? { method: "GET", headers: authH }
      : { method: "POST", headers: { ...authH, "Content-Type": "application/json" }, body: JSON.stringify(body) };
  const r = await fetch(path, opts);
  const data = await r.json().catch(() => ({}));
  if (r.status === 401) {
    keyEl.classList.add("needs-key");
    keyEl.focus();
    toast(t("auth.need"));
    throw new Error(t("auth.need"));
  }
  if (!r.ok) throw new Error(data.error || `HTTP ${r.status}`);
  keyEl.classList.remove("needs-key");
  return data;
}

function toast(msg) {
  const el = $("toast");
  el.textContent = msg;
  el.hidden = false;
  clearTimeout(el._timer);
  el._timer = setTimeout(() => (el.hidden = true), 4000);
}

function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

function timeAgo(ts) {
  if (!ts) return "–";
  const d = Date.now() / 1000 - ts;
  if (d < 60) return t("timeNow");
  if (d < 3600) return t("timeM")(Math.floor(d / 60));
  if (d < 86400) return t("timeH")(Math.floor(d / 3600));
  return t("timeD")(Math.floor(d / 86400));
}

/* ---------- tabs ---------- */
document.querySelectorAll(".tab").forEach((b) =>
  b.addEventListener("click", () => {
    document.querySelectorAll(".tab").forEach((x) => x.classList.toggle("active", x === b));
    document.querySelectorAll(".view").forEach((v) => v.classList.toggle("active", v.id === "view-" + b.dataset.view));
    if (b.dataset.view === "audit") loadAudit();
    if (b.dataset.view === "team") loadTeam();
    if (b.dataset.view === "overview") loadOverview();
    if (b.dataset.view === "graph") loadGraph();
    else stopSim();
  })
);

/* ---------- audit (L2.3) ---------- */
async function loadAudit() {
  try {
    const action = $("audit-action").value;
    const d = await api(`/v1/audit?limit=300${action ? `&action=${encodeURIComponent(action)}` : ""}`);
    const rows = d.events || [];
    $("audit-rows").innerHTML = rows.map((e) => `
      <tr>
        <td class="muted" title="${new Date(e.ts * 1000).toLocaleString()}">${timeAgo(e.ts)}</td>
        <td><span class="rel-chip${e.action === "denied" ? " denied" : ""}">${esc(e.action)}</span></td>
        <td class="mono">${esc(e.tenant)}</td>
        <td class="mono">${esc(e.owner)}</td>
        <td class="mono">${esc(e.actor || "")}</td>
        <td class="fact-cell" title="${esc(e.detail)}">${esc(e.detail)}</td>
      </tr>`).join("");
    $("audit-empty").hidden = rows.length > 0;
    $("audit-total").textContent = rows.length ? `${rows.length}` : "";
  } catch (e) { toast(e.message); }
}
$("audit-refresh").addEventListener("click", loadAudit);
$("audit-action").addEventListener("change", loadAudit);

/* ---------- team（key 管理 + 成员活动） ---------- */
async function loadTeam() {
  try {
    const d = await api("/v1/keys");
    const rows = d.keys || [];
    $("team-empty").hidden = true;
    $("team-rows").innerHTML = rows.map((k) => {
      const prefix = k.key.replace(/…$/, "");
      return `
      <tr>
        <td class="mono">${esc(k.key)}</td>
        <td>${esc(k.name || "–")}</td>
        <td class="mono">${esc(k.tenant)}</td>
        <td><span class="rel-chip">${esc(k.scope)}</span></td>
        <td><button class="btn danger team-revoke" data-prefix="${esc(prefix)}" data-i18n-title="tip.delete" title="revoke">×</button></td>
      </tr>`;
    }).join("");
    document.querySelectorAll(".team-revoke").forEach((btn) =>
      btn.addEventListener("click", async () => {
        const p = btn.dataset.prefix;
        if (!confirm(t("team.revokeConfirm")(p + "…"))) return;
        try {
          await api(`/v1/keys/${encodeURIComponent(p)}`, undefined, "DELETE");
          toast(t("team.revoked")(p + "…"));
          loadTeam();
        } catch (e) { toast(e.message); }
      })
    );
  } catch (e) {
    $("team-rows").innerHTML = "";
    const empty = $("team-empty");
    empty.hidden = false;
    empty.textContent = e.message;
  }
  // 成员活动：审计事件按 actor 聚合
  try {
    const d = await api("/v1/audit?limit=300");
    const byActor = {};
    (d.events || []).forEach((e) => {
      if (!e.actor) return;
      byActor[e.actor] = byActor[e.actor] || { total: 0, last: 0, actions: {} };
      const a = byActor[e.actor];
      a.total += 1;
      a.last = Math.max(a.last, e.ts);
      a.actions[e.action] = (a.actions[e.action] || 0) + 1;
    });
    const names = Object.keys(byActor).sort((x, y) => byActor[y].total - byActor[x].total);
    $("team-activity").innerHTML = names.length
      ? names.map((n) => {
          const a = byActor[n];
          const top = Object.entries(a.actions).sort((p, q) => q[1] - p[1]).slice(0, 3)
            .map(([act, c]) => `${esc(act)}×${c}`).join(" · ");
          return `<div class="ov-item"><b>${esc(n)}</b> <span class="muted">${a.total} ops · ${top} · ${timeAgo(a.last)}</span></div>`;
        }).join("")
      : `<div class="muted">${esc(t("team.noActivity"))}</div>`;
  } catch (e) { /* 审计不可用时静默（开放模式也无妨） */ }
}

$("team-add").addEventListener("click", async () => {
  const name = $("team-name").value.trim();
  const tenant = $("team-tenant").value.trim();
  const scope = $("team-scope").value;
  if (!tenant) { toast(t("ph.teamTenant")); return; }
  try {
    const d = await api("/v1/keys", { tenant, scope, name: name || undefined });
    const box = $("team-newkey");
    box.hidden = false;
    box.innerHTML = `<div class="muted">${esc(t("team.newKeyHint"))}</div>
      <div class="mono team-key-full">${esc(d.key)}</div>
      <button class="btn" id="team-copy">${esc(t("team.copy"))}</button>`;
    $("team-copy").addEventListener("click", async (ev) => {
      try {
        await navigator.clipboard.writeText(d.key);
        ev.target.textContent = t("team.copied");
      } catch (e) { toast(e.message); }
    });
    toast(t("team.issued"));
    loadTeam();
  } catch (e) { toast(e.message); }
});

/* ---------- stats ---------- */
async function loadStats() {
  try {
    const s = await api("/v1/stats");
    $("stat-nodes").textContent = s.nodes;
    $("stat-edges").textContent = s.edges;
    const em = $("stat-embed"), ll = $("stat-llm");
    const degraded = s.embedder_status === "degraded";
    em.textContent = !s.embedder ? t("embedOff")
      : degraded ? t("embedDegraded")(s.embed_dims, s.embed_failures)
      : t("embedOn")(s.embed_dims);
    em.classList.toggle("on", !!s.embedder && !degraded);
    em.classList.toggle("warn", degraded);
    em.title = degraded && s.embedder_last_error ? s.embedder_last_error : em.title;
    ll.textContent = s.llm ? t("llmOn") : t("llmOff");
    ll.classList.toggle("on", !!s.llm);
  } catch (e) { /* engine unreachable — keep placeholders */ }
}

/* ---------- memories ---------- */
const PAGE = 50;
const state = { page: 0, total: 0, rows: [] };

const scopeEl = $("mem-scope");
scopeEl.value = localStorage.getItem("nylon.scope") || "all";
scopeEl.addEventListener("change", () => {
  localStorage.setItem("nylon.scope", scopeEl.value);
  state.page = 0;
  loadMemories();
});

async function loadMemories() {
  try {
    const ownerParam = scopeEl.value === "mine" ? `&owner=${encodeURIComponent(owner())}` : "";
    const d = await api(`/v1/nodes?offset=${state.page * PAGE}&limit=${PAGE}${ownerParam}`);
    state.total = d.total;
    state.rows = d.nodes;
    renderMemories();
  } catch (e) { toast(e.message); }
}

function renderMemories() {
  const q = $("mem-filter").value.trim().toLowerCase();
  const rows = state.rows.filter((n) => !q || n.fact.toLowerCase().includes(q));
  const tb = $("mem-rows");
  tb.innerHTML = rows.map((n) => `
    <tr data-id="${n.id}">
      <td class="num">${n.id}</td>
      <td class="fact-cell" title="${esc(n.fact)}">${esc(n.fact)}</td>
      <td><div class="tbar"><i style="width:${Math.min(100, n.tension * 100).toFixed(0)}%"></i></div><span class="tval">${n.tension.toFixed(3)}</span></td>
      <td>${n.relations.slice(0, 3).map((r) => `<span class="rel-chip">${esc(r)}</span>`).join("")}</td>
      <td class="num">${n.mentions_7d}</td>
      <td class="muted">${timeAgo(n.created_at)}</td>
    </tr>`).join("");
  $("mem-empty").hidden = rows.length > 0;
  $("mem-total").textContent = t("memTotal")(state.total);
  $("mem-page").textContent = String(state.page + 1);
  tb.querySelectorAll("tr").forEach((tr) => tr.addEventListener("click", () => openDrawer(+tr.dataset.id)));
}

$("mem-filter").addEventListener("input", renderMemories);
$("mem-refresh").addEventListener("click", () => { loadStats(); loadMemories(); });
$("mem-prev").addEventListener("click", () => { if (state.page > 0) { state.page--; loadMemories(); } });
$("mem-next").addEventListener("click", () => { if ((state.page + 1) * PAGE < state.total) { state.page++; loadMemories(); } });

/* ---------- drawer ---------- */
let drawerNodeId = null;
async function openDrawer(id) {
  try {
    const n = await api(`/v1/nodes/${id}`);
    const f = n.filaments || {};
    drawerNodeId = n.node_id;
    $("d-id").textContent = "#" + n.node_id;
    $("d-body").innerHTML = `
      <div class="d-fact">${esc(f.fact || "")}</div>
      <div class="muted">${t("dTension")}</div>
      <div class="d-tension">${(n.current_tension ?? 0).toFixed(4)}</div>
      <dl class="d-grid">
        <dt>${t("dRelations")}</dt><dd>${(f.relations || []).map(esc).join(", ") || "–"}</dd>
        <dt>${t("dValence")}</dt><dd>${f.emotion_valence ?? "–"}</dd>
        <dt>${t("dIntensity")}</dt><dd>${f.emotion_intensity ?? "–"}</dd>
        <dt>${t("dConfidence")}</dt><dd>${f.confidence ?? "–"}</dd>
        <dt>${t("dDecay")}</dt><dd>${f.decay_rate ?? "–"} ${t("dPerDay")}</dd>
        <dt>${t("dMentions")}</dt><dd>${f.mentions_7d ?? "–"}</dd>
        <dt>${t("dCreated")}</dt><dd>${f.created_at ? new Date(f.created_at * 1000).toLocaleString() : "–"}</dd>
      </dl>`;
    $("drawer").hidden = false;
  } catch (e) { toast(e.message); }
}
$("d-close").addEventListener("click", () => ($("drawer").hidden = true));
document.addEventListener("keydown", (e) => { if (e.key === "Escape") $("drawer").hidden = true; });

/* ---------- resonate ---------- */
$("res-run").addEventListener("click", async () => {
  const q = $("res-query").value.trim();
  if (!q) return;
  const hops = $("res-hops").value;
  $("res-results").innerHTML = "";
  $("res-meta").textContent = t("resRunning");
  try {
    const d = await api("/v1/resonate", {
      owner_id: owner(),
      query: q,
      budget: +$("res-budget").value || 0,
      ...(hops !== "" ? { max_hops: +hops } : {}),
    });
    const seeds = new Set(d.seed_ids || []);
    $("res-meta").textContent = t("resMeta")(d.activated.length, [...seeds].join(", "));
    const max = Math.max(...d.activated.map((a) => a.resonance), 1e-9);
    $("res-results").innerHTML = d.activated.map((a, i) => `
      <div class="card" data-id="${a.node_id}">
        <div class="head">
          <span class="rank">${i + 1}</span>
          <span class="fact-cell">${esc(a.filaments?.fact || "")}</span>
          ${seeds.has(a.node_id) ? '<span class="badge">seed</span>' : ""}
          <span class="score">${a.resonance.toFixed(3)}</span>
        </div>
        <div class="bar"><i style="width:${(a.resonance / max * 100).toFixed(1)}%"></i></div>
      </div>`).join("") || `<div class="empty muted">${t("resEmpty")}</div>`;
    document.querySelectorAll("#res-results .card").forEach((c) =>
      c.addEventListener("click", () => openDrawer(+c.dataset.id)));
    // 记录本次检索上下文，展示反馈条（回答质量回执 → 反馈驱动反思）
    lastResonate = { query: q, shown: d.activated.map((a) => a.node_id) };
    $("res-feedback").hidden = false;
    $("fb-status").textContent = "";
  } catch (e) { $("res-meta").textContent = ""; toast(e.message); }
});
$("res-query").addEventListener("keydown", (e) => { if (e.key === "Enter") $("res-run").click(); });

/* ---------- weave ---------- */
$("wv-run").addEventListener("click", async () => {
  const text = $("wv-text").value.trim();
  if (!text) return;
  try {
    const d = await api("/v1/weave", {
      owner_id: owner(), raw_event: text,
      ...($("wv-task").value.trim() ? { task: $("wv-task").value.trim() } : {}),
    });
    $("wv-result").hidden = false;
    $("wv-result").innerHTML = `
      <div class="kv"><b>${t("kvNode")}</b><a data-id="${d.node_id}">#${d.node_id}</a></div>
      <div class="kv"><b>${t("kvLinked")}</b><span class="mono">${d.linked_nodes.join(", ") || "–"}</span></div>
      <div class="kv"><b>${t("kvConflicts")}</b><span class="mono">${d.conflict_nodes.join(", ") || "–"}</span></div>`;
    $("wv-result").querySelector("a").addEventListener("click", (e) => openDrawer(+e.target.dataset.id));
    $("wv-text").value = "";
    loadStats(); loadMemories();
  } catch (e) { toast(e.message); }
});

$("ws-run").addEventListener("click", async () => {
  let events;
  try { events = JSON.parse($("ws-text").value); if (!Array.isArray(events)) throw 0; }
  catch { toast(t("errJson")); return; }
  try {
    const d = await api("/v1/weave_session", {
      owner_id: owner(), events, skip_abstract: $("ws-skip").checked,
    });
    $("ws-result").hidden = false;
    $("ws-result").innerHTML = `
      <div class="kv"><b>${t("kvLeaf")}</b><span class="mono">${d.leaf_nodes.map((l) => `${l.event_id || "?"}→#${l.node_id}`).join(", ") || "–"}</span></div>
      <div class="kv"><b>${t("kvFact")}</b><span class="mono">${d.fact_nodes.map((f) => "#" + f.node_id).join(", ") || t("factSkipped")}</span></div>`;
    $("ws-text").value = "";
    loadStats(); loadMemories();
  } catch (e) { toast(e.message); }
});

/* ---------- overview ---------- */
async function loadOverview() {
  try {
    const s = await api("/v1/stats");
    $("ov-nodes").textContent = s.nodes;
    $("ov-edges").textContent = s.edges;
    const oe = $("ov-embed"), ol = $("ov-llm");
    const degraded = s.embedder_status === "degraded";
    oe.textContent = !s.embedder ? "off" : degraded ? `${s.embed_dims}d!` : `${s.embed_dims}d`;
    oe.classList.toggle("off", !s.embedder);
    oe.style.color = degraded ? "#f59e0b" : "";
    ol.textContent = s.llm ? "on" : "off";
    ol.classList.toggle("off", !s.llm);
  } catch (e) { /* keep placeholders */ }
  try {
    const d = await api("/v1/nodes?limit=8");
    $("ov-memories").innerHTML = d.nodes.map((n) => `
      <div class="ov-item" data-id="${n.id}">
        <span class="fact-cell">${esc(n.fact)}</span>
        <span class="muted">${timeAgo(n.created_at)}</span>
      </div>`).join("") || `<div class="empty muted">${t("ov.empty")}</div>`;
    $("ov-memories").querySelectorAll(".ov-item").forEach((el) =>
      el.addEventListener("click", () => openDrawer(+el.dataset.id)));
  } catch (e) { /* auth/offline */ }
  try {
    const d = await api("/v1/audit?limit=8");
    const rows = d.events || [];
    $("ov-activity").innerHTML = rows.map((e) => `
      <div class="ov-item">
        <span class="fact-cell"><span class="rel-chip${e.action === "denied" ? " denied" : ""}">${esc(e.action)}</span> ${esc(e.detail)}</span>
        <span class="muted">${timeAgo(e.ts)}</span>
      </div>`).join("") || `<div class="empty muted">${t("ov.empty")}</div>`;
  } catch (e) { /* auth/offline */ }
  try {
    const d = await api("/v1/nodes?limit=500");
    drawHist(d.nodes.map((n) => n.tension));
    $("ov-hist-note").textContent = t("ovHistNote")(d.total, d.nodes.length);
  } catch (e) { /* auth/offline */ }
}

function drawHist(tensions) {
  const c = $("ov-hist");
  const ctx = c.getContext("2d");
  const W = 360, H = 140, dpr = window.devicePixelRatio || 1;
  c.width = W * dpr; c.height = H * dpr;
  c.style.width = W + "px"; c.style.height = H + "px";
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  const css = getComputedStyle(document.documentElement);
  const accent = css.getPropertyValue("--accent").trim() || "#2dd4bf";
  const muted = css.getPropertyValue("--muted").trim() || "#8b929e";
  ctx.clearRect(0, 0, W, H);
  const B = 10, buckets = new Array(B).fill(0);
  tensions.forEach((v) => buckets[Math.min(B - 1, Math.max(0, Math.floor(v * B)))]++);
  const max = Math.max(...buckets, 1);
  const bw = (W - 20) / B;
  ctx.fillStyle = accent;
  buckets.forEach((v, i) => {
    const h = (H - 36) * v / max;
    ctx.globalAlpha = v ? 0.9 : 0.12;
    ctx.fillRect(10 + i * bw + 2, H - 24 - h, bw - 4, Math.max(h, 1));
  });
  ctx.globalAlpha = 1;
  ctx.fillStyle = muted;
  ctx.font = "10px sans-serif";
  ctx.fillText("0", 10, H - 8);
  ctx.fillText("tension →", W / 2 - 22, H - 8);
  const t1 = "1.0";
  ctx.fillText(t1, W - 10 - ctx.measureText(t1).width, H - 8);
}

/* ---------- graph view ---------- */
const gcanvas = $("graph-canvas");
const gtip = $("graph-tip");
let gsim = null;

const OWNER_HUES = [174, 36, 265, 330, 110, 200, 15, 80, 290, 130];

function graphColors() {
  const css = getComputedStyle(document.documentElement);
  return {
    edge: css.getPropertyValue("--muted").trim() || "#8b929e",
    text: css.getPropertyValue("--text").trim() || "#e6e8eb",
  };
}

async function loadGraph() {
  stopSim();
  try {
    const limit = +$("graph-limit").value || 300;
    const d = await api(`/v1/graph?limit=${limit}`);
    $("graph-meta").textContent = t("graphMeta")(d.nodes.length, d.total, d.edges.length);
    if (!d.nodes.length) {
      $("graph-meta").textContent = t("graph.empty");
      return;
    }
    startSim(d);
  } catch (e) { toast(e.message); }
}

function stopSim() {
  if (gsim && gsim.raf) cancelAnimationFrame(gsim.raf);
  gsim = null;
  if (gtip) gtip.hidden = true;
}

function startSim(data) {
  const wrap = gcanvas.parentElement;
  const W = Math.max(300, wrap.clientWidth);
  const H = Math.max(320, wrap.clientHeight);
  const dpr = window.devicePixelRatio || 1;
  gcanvas.width = W * dpr; gcanvas.height = H * dpr;
  gcanvas.style.width = W + "px"; gcanvas.style.height = H + "px";
  //  owner → hue 映射（按出现顺序取色板）
  const ownerHues = new Map();
  data.nodes.forEach((n) => {
    if (!ownerHues.has(n.owner_id)) ownerHues.set(n.owner_id, OWNER_HUES[ownerHues.size % OWNER_HUES.length]);
  });
  const nodes = data.nodes.map((n, i) => ({
    ...n,
    // 初始位置：类叶序散布，避免全部叠在圆心
    x: W / 2 + Math.cos(i * 2.4) * (30 + 8 * Math.sqrt(i)),
    y: H / 2 + Math.sin(i * 2.4) * (30 + 8 * Math.sqrt(i)),
    vx: 0, vy: 0,
  }));
  const idx = new Map(nodes.map((n, i) => [n.id, i]));
  const edges = data.edges
    .map((e) => ({ a: idx.get(e.from), b: idx.get(e.to), w: e.weight }))
    .filter((e) => e.a !== undefined && e.b !== undefined);
  gsim = { nodes, edges, W, H, dpr, hover: -1, drag: -1, moved: false, colors: graphColors(), ownerHues };
  gsim.raf = requestAnimationFrame(stepSim);
}

function colorOfNode(p) {
  if ($("graph-color").value === "owner") {
    return `hsl(${gsim.ownerHues.get(p.owner_id) ?? 174} 60% 55%)`;
  }
  const v = Math.max(0, Math.min(1, p.tension));
  return `hsl(${174 - v * 138} 65% 50%)`; // 低张力 teal → 高张力 amber
}

function stepSim() {
  if (!gsim) return;
  const { nodes, edges, W, H } = gsim;
  const n = nodes.length;
  // 斥力（截断半径 200px，O(n²) 在 600 节点内可接受）
  for (let i = 0; i < n; i++) {
    const a = nodes[i];
    for (let j = i + 1; j < n; j++) {
      const b = nodes[j];
      let dx = a.x - b.x, dy = a.y - b.y;
      const d2 = dx * dx + dy * dy + 0.01;
      if (d2 > 40000) continue;
      const d = Math.sqrt(d2);
      const f = 900 / d2;
      dx /= d; dy /= d;
      a.vx += dx * f; a.vy += dy * f;
      b.vx -= dx * f; b.vy -= dy * f;
    }
  }
  // 弹簧（边权越重越紧）
  for (const e of edges) {
    const a = nodes[e.a], b = nodes[e.b];
    const dx = b.x - a.x, dy = b.y - a.y;
    const d = Math.sqrt(dx * dx + dy * dy) || 0.01;
    const f = (d - 70) * 0.02 * Math.min(1, e.w + 0.2);
    const fx = (dx / d) * f, fy = (dy / d) * f;
    a.vx += fx; a.vy += fy; b.vx -= fx; b.vy -= fy;
  }
  // 向心引力 + 阻尼
  for (const p of nodes) {
    p.vx += (W / 2 - p.x) * 0.002;
    p.vy += (H / 2 - p.y) * 0.002;
    p.vx *= 0.85; p.vy *= 0.85;
    if (gsim.drag !== p.id) { p.x += p.vx; p.y += p.vy; }
  }
  drawGraph();
  gsim.raf = requestAnimationFrame(stepSim);
}

function drawGraph() {
  const { nodes, edges, W, H, dpr } = gsim;
  const ctx = gcanvas.getContext("2d");
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, W, H);
  ctx.lineWidth = 1;
  for (const e of edges) {
    const a = nodes[e.a], b = nodes[e.b];
    ctx.strokeStyle = gsim.colors.edge;
    ctx.globalAlpha = 0.1 + 0.45 * Math.min(1, e.w);
    ctx.beginPath(); ctx.moveTo(a.x, a.y); ctx.lineTo(b.x, b.y); ctx.stroke();
  }
  ctx.globalAlpha = 1;
  for (const p of nodes) {
    const r = 3.5 + Math.max(0, Math.min(1, p.tension)) * 3.5;
    ctx.fillStyle = colorOfNode(p);
    ctx.beginPath(); ctx.arc(p.x, p.y, r, 0, 6.2832); ctx.fill();
    if (p.id === gsim.hover) {
      ctx.strokeStyle = gsim.colors.text; ctx.lineWidth = 1.5;
      ctx.beginPath(); ctx.arc(p.x, p.y, r + 2.5, 0, 6.2832); ctx.stroke();
    }
  }
}

function pickNode(x, y) {
  if (!gsim) return null;
  let best = null, bestD = 1e9;
  for (const p of gsim.nodes) {
    const r = 3.5 + Math.max(0, Math.min(1, p.tension)) * 3.5 + 3;
    const d = (p.x - x) * (p.x - x) + (p.y - y) * (p.y - y);
    if (d < r * r && d < bestD) { best = p; bestD = d; }
  }
  return best;
}

gcanvas.addEventListener("mousedown", (e) => {
  if (!gsim) return;
  const r = gcanvas.getBoundingClientRect();
  const p = pickNode(e.clientX - r.left, e.clientY - r.top);
  if (p) { gsim.drag = p.id; gsim.moved = false; }
});
gcanvas.addEventListener("mousemove", (e) => {
  if (!gsim) return;
  const r = gcanvas.getBoundingClientRect();
  const x = e.clientX - r.left, y = e.clientY - r.top;
  if (gsim.drag >= 0) {
    const p = gsim.nodes.find((n) => n.id === gsim.drag);
    if (p) { p.x = x; p.y = y; p.vx = 0; p.vy = 0; gsim.moved = true; }
    return;
  }
  const p = pickNode(x, y);
  gsim.hover = p ? p.id : -1;
  gcanvas.style.cursor = p ? "pointer" : "default";
  if (p) {
    gtip.hidden = false;
    gtip.innerHTML = `<b>#${p.id}</b> · T=${p.tension.toFixed(3)} · ${esc(p.owner_id)}<br>${esc(p.fact)}`;
    const tw = gtip.offsetWidth, th = gtip.offsetHeight;
    gtip.style.left = Math.min(x + 14, gsim.W - tw - 8) + "px";
    gtip.style.top = Math.min(y + 14, gsim.H - th - 8) + "px";
  } else {
    gtip.hidden = true;
  }
});
gcanvas.addEventListener("mouseup", (e) => {
  if (!gsim) return;
  const wasDrag = gsim.drag >= 0 && gsim.moved;
  const dragId = gsim.drag;
  gsim.drag = -1;
  if (!wasDrag && dragId < 0) {
    const r = gcanvas.getBoundingClientRect();
    const p = pickNode(e.clientX - r.left, e.clientY - r.top);
    if (p) openDrawer(p.id);
  }
});
gcanvas.addEventListener("mouseleave", () => {
  if (!gsim) return;
  gsim.drag = -1; gsim.hover = -1; gtip.hidden = true;
});
$("graph-refresh").addEventListener("click", loadGraph);
$("graph-limit").addEventListener("change", loadGraph);
$("graph-color").addEventListener("change", () => { if (gsim) gsim.colors = graphColors(); });

/* ---------- resonate feedback ---------- */
let lastResonate = null;
document.querySelectorAll("#res-feedback .fb").forEach((b) =>
  b.addEventListener("click", async () => {
    if (!lastResonate) return;
    try {
      await api("/v1/feedback", {
        owner_id: owner(),
        query: lastResonate.query,
        rating: b.dataset.rating,
        shown_node_ids: lastResonate.shown,
      });
      $("fb-status").textContent = t("fbThanks");
    } catch (e) { toast(e.message); }
  })
);

/* ---------- drawer delete ---------- */
$("d-delete").addEventListener("click", async () => {
  if (drawerNodeId == null) return;
  if (!confirm(t("delConfirm")(drawerNodeId))) return;
  try {
    await api(`/v1/nodes/${drawerNodeId}`, null, "DELETE");
    toast(t("delDone")(drawerNodeId));
    $("drawer").hidden = true;
    drawerNodeId = null;
    loadStats(); loadMemories();
    if (gsim) loadGraph();
  } catch (e) { toast(e.message); }
});

/* deep link: /#resonate or /#weave opens that view */
if (location.hash) {
  const v = location.hash.slice(1);
  const tab = document.querySelector(`.tab[data-view="${v}"]`);
  if (tab) tab.click();
}

applyI18n();
loadStats();
loadMemories();
loadOverview();

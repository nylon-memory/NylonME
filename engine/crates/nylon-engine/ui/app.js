/* NylonME Console — zero-build embedded UI. Talks to the in-binary REST API. */
"use strict";

const $ = (id) => document.getElementById(id);

/* ---------- i18n ---------- */
const I18N = {
  en: {
    "stats.nodes": "nodes", "stats.edges": "edges",
    "tab.memories": "Memories", "tab.resonate": "Resonate", "tab.weave": "Weave",
    "tab.audit": "Audit",
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
    llmOn: "llm on", llmOff: "llm off",
    memTotal: (n) => `${n} nodes`,
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
    "tab.memories": "记忆", "tab.resonate": "共振", "tab.weave": "编织",
    "tab.audit": "审计",
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
    llmOn: "LLM 开", llmOff: "LLM 关",
    memTotal: (n) => `${n} 条记忆`,
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

async function api(path, body) {
  const opts = body === undefined
    ? { method: "GET" }
    : { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) };
  const r = await fetch(path, opts);
  const data = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(data.error || `HTTP ${r.status}`);
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
        <td class="fact-cell" title="${esc(e.detail)}">${esc(e.detail)}</td>
      </tr>`).join("");
    $("audit-empty").hidden = rows.length > 0;
    $("audit-total").textContent = rows.length ? `${rows.length}` : "";
  } catch (e) { toast(e.message); }
}
$("audit-refresh").addEventListener("click", loadAudit);
$("audit-action").addEventListener("change", loadAudit);

/* ---------- stats ---------- */
async function loadStats() {
  try {
    const s = await api("/v1/stats");
    $("stat-nodes").textContent = s.nodes;
    $("stat-edges").textContent = s.edges;
    const em = $("stat-embed"), ll = $("stat-llm");
    em.textContent = s.embedder ? t("embedOn")(s.embed_dims) : t("embedOff");
    em.classList.toggle("on", !!s.embedder);
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
async function openDrawer(id) {
  try {
    const n = await api(`/v1/nodes/${id}`);
    const f = n.filaments || {};
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

/* deep link: /#resonate or /#weave opens that view */
if (location.hash) {
  const v = location.hash.slice(1);
  const tab = document.querySelector(`.tab[data-view="${v}"]`);
  if (tab) tab.click();
}

applyI18n();
loadStats();
loadMemories();

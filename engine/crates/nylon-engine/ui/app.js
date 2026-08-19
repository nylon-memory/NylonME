/* NylonME Console — zero-build embedded UI. Talks to the in-binary REST API. */
"use strict";

const $ = (id) => document.getElementById(id);
const ownerEl = $("owner");
ownerEl.value = localStorage.getItem("nylon.owner") || "default";
ownerEl.addEventListener("change", () => {
  localStorage.setItem("nylon.owner", ownerEl.value.trim() || "default");
  state.page = 0;
  loadStats();
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
  const t = $("toast");
  t.textContent = msg;
  t.hidden = false;
  clearTimeout(t._timer);
  t._timer = setTimeout(() => (t.hidden = true), 4000);
}

function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

function timeAgo(ts) {
  if (!ts) return "–";
  const d = Date.now() / 1000 - ts;
  if (d < 60) return "just now";
  if (d < 3600) return Math.floor(d / 60) + "m ago";
  if (d < 86400) return Math.floor(d / 3600) + "h ago";
  return Math.floor(d / 86400) + "d ago";
}

/* ---------- tabs ---------- */
document.querySelectorAll(".tab").forEach((b) =>
  b.addEventListener("click", () => {
    document.querySelectorAll(".tab").forEach((x) => x.classList.toggle("active", x === b));
    document.querySelectorAll(".view").forEach((v) => v.classList.toggle("active", v.id === "view-" + b.dataset.view));
  })
);

/* ---------- stats ---------- */
async function loadStats() {
  try {
    const s = await api("/v1/stats");
    $("stat-nodes").textContent = s.nodes;
    $("stat-edges").textContent = s.edges;
    const em = $("stat-embed"), ll = $("stat-llm");
    em.textContent = s.embedder ? `embed ${s.embed_dims}d` : "embed off";
    em.classList.toggle("on", !!s.embedder);
    ll.textContent = s.llm ? "llm on" : "llm off";
    ll.classList.toggle("on", !!s.llm);
  } catch (e) { /* engine unreachable — keep placeholders */ }
}

/* ---------- memories ---------- */
const PAGE = 50;
const state = { page: 0, total: 0, rows: [] };

async function loadMemories() {
  try {
    const d = await api(`/v1/nodes?owner=${encodeURIComponent(owner())}&offset=${state.page * PAGE}&limit=${PAGE}`);
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
  $("mem-total").textContent = `${state.total} nodes`;
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
      <div class="muted">current tension</div>
      <div class="d-tension">${(n.current_tension ?? 0).toFixed(4)}</div>
      <dl class="d-grid">
        <dt>relations</dt><dd>${(f.relations || []).map(esc).join(", ") || "–"}</dd>
        <dt>valence</dt><dd>${f.emotion_valence ?? "–"}</dd>
        <dt>intensity</dt><dd>${f.emotion_intensity ?? "–"}</dd>
        <dt>confidence</dt><dd>${f.confidence ?? "–"}</dd>
        <dt>decay rate</dt><dd>${f.decay_rate ?? "–"} /day</dd>
        <dt>mentions 7d</dt><dd>${f.mentions_7d ?? "–"}</dd>
        <dt>created</dt><dd>${f.created_at ? new Date(f.created_at * 1000).toLocaleString() : "–"}</dd>
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
  $("res-meta").textContent = "resonating…";
  try {
    const d = await api("/v1/resonate", {
      owner_id: owner(),
      query: q,
      budget: +$("res-budget").value || 0,
      ...(hops !== "" ? { max_hops: +hops } : {}),
    });
    const seeds = new Set(d.seed_ids || []);
    $("res-meta").textContent = `${d.activated.length} activated · seeds [${[...seeds].join(", ")}]`;
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
      </div>`).join("") || '<div class="empty muted">nothing resonated</div>';
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
      <div class="kv"><b>node</b><a data-id="${d.node_id}">#${d.node_id}</a></div>
      <div class="kv"><b>linked</b><span class="mono">${d.linked_nodes.join(", ") || "–"}</span></div>
      <div class="kv"><b>conflicts</b><span class="mono">${d.conflict_nodes.join(", ") || "–"}</span></div>`;
    $("wv-result").querySelector("a").addEventListener("click", (e) => openDrawer(+e.target.dataset.id));
    $("wv-text").value = "";
    loadStats(); loadMemories();
  } catch (e) { toast(e.message); }
});

$("ws-run").addEventListener("click", async () => {
  let events;
  try { events = JSON.parse($("ws-text").value); if (!Array.isArray(events)) throw 0; }
  catch { toast("invalid JSON array of events"); return; }
  try {
    const d = await api("/v1/weave_session", {
      owner_id: owner(), events, skip_abstract: $("ws-skip").checked,
    });
    $("ws-result").hidden = false;
    $("ws-result").innerHTML = `
      <div class="kv"><b>leaf nodes</b><span class="mono">${d.leaf_nodes.map((l) => `${l.event_id || "?"}→#${l.node_id}`).join(", ") || "–"}</span></div>
      <div class="kv"><b>fact nodes</b><span class="mono">${d.fact_nodes.map((f) => "#" + f.node_id).join(", ") || "– (abstract layer skipped)"}</span></div>`;
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

loadStats();
loadMemories();
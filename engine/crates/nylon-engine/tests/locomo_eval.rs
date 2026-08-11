//! LoCoMo 子集检索评测：把多轮对话逐轮织入引擎，对 QA 问题跑 Resonate，
//! 统计 gold evidence 轮次是否出现在激活结果前 10（recall@10）。
//!
//! 数据集：https://github.com/snap-research/locomo （data/locomo10.json）
//! 用法：
//!   $env:NYLON_LOCOMO_PATH="D:\data\locomo10.json"
//!   cargo test --release -p nylon-engine --test locomo_eval -- --ignored --nocapture
//! 语义口径：再加 NYLON_EMBED_URL / NYLON_EMBED_MODEL / NYLON_EMBED_DIMS（如本地 ollama bge-m3）
//!
//! 口径说明：Phase 1 的 Resonate 种子是词面检索（嵌入模型未接入），本评测度量
//! 检索/激活层的证据召回率，不是端到端 QA 准确率（后者需要 LLM 生成 + 裁判）。
//! category=5 为对抗题（答案"未提及"），不计入。

#[path = "../src/service.rs"]
mod service;

use nylon_llm::llm_from_env;
use nylon_storage::PersistentGraph;
use service::pb::memory_engine_client::MemoryEngineClient;
use service::pb::memory_engine_server::MemoryEngineServer;
use service::pb::*;
use service::EngineService;
use std::collections::HashMap;

const RECALL_K: usize = 10;

/// 逐轮原文编织（叶子层）：raw_event = "speaker: text"，dia_id -> 节点映射。
async fn weave_turns(
    client: &mut MemoryEngineClient<tonic::transport::Channel>,
    sample: &str,
    turns: &[serde_json::Value],
    dia2nodes: &mut HashMap<String, Vec<u64>>,
    total_turns: &mut usize,
) {
    for turn in turns {
        let dia = turn["dia_id"].as_str().unwrap_or("").to_string();
        let speaker = turn["speaker"].as_str().unwrap_or("");
        let text = turn["text"].as_str().unwrap_or("");
        if dia.is_empty() || text.is_empty() {
            continue;
        }
        let resp = client
            .weave(WeaveRequest {
                tenant_id: "locomo".into(),
                owner_id: sample.to_string(),
                raw_event: format!("{speaker}: {text}"),
                context: None,
            })
            .await
            .unwrap()
            .into_inner();
        dia2nodes.entry(dia).or_default().push(resp.node_id);
        *total_turns += 1;
        if *total_turns % 50 == 0 {
            println!("[eval] weave 进度 {total_turns} 条");
        }
    }
}

/// Session 级事实编织（抽象层）：整段 session 一次性交给 LLM 分解为原子事实
///（指代消解到规范名），逐条 weave 入库；source dia_id 追加映射到事实节点。
async fn weave_session_facts(
    client: &mut MemoryEngineClient<tonic::transport::Channel>,
    llm: &Option<std::sync::Arc<dyn nylon_llm::ChatModel>>,
    sample: &str,
    turns: &[serde_json::Value],
    dia2nodes: &mut HashMap<String, Vec<u64>>,
    total_turns: &mut usize,
) {
    let mut lines: Vec<String> = Vec::new();
    for turn in turns {
        let dia = turn["dia_id"].as_str().unwrap_or("");
        let speaker = turn["speaker"].as_str().unwrap_or("");
        let text = turn["text"].as_str().unwrap_or("");
        if !dia.is_empty() && !text.is_empty() {
            lines.push(format!("{dia} {speaker}: {text}"));
        }
    }
    if lines.is_empty() {
        return;
    }
    let Some(llm) = llm else { return };
    let system = "You are a memory extraction engine. Given a dialogue session with turn IDs, extract atomic factual memories worth remembering long-term. Resolve pronouns and partial names to canonical full names (e.g. 'she' -> the person's name). Merge duplicate information. Preserve exact details: dates, numbers, places, names. Each fact must be self-contained. Output ONLY valid JSON: {\"facts\": [{\"fact\": \"...\", \"source\": [\"dia_id\", ...]}]}. Skip greetings and small talk without facts.";
    let facts = match llm.chat_json(system, &lines.join("\n")).await {
        Ok(v) => v.get("facts").and_then(|f| f.as_array()).cloned().unwrap_or_default(),
        Err(e) => {
            eprintln!("[eval] session 分解失败，本段跳过事实层: {e}");
            return;
        }
    };
    for f in facts {
        let fact = f.get("fact").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
        if fact.is_empty() {
            continue;
        }
        let resp = client
            .weave(WeaveRequest {
                tenant_id: "locomo".into(),
                owner_id: sample.to_string(),
                raw_event: fact,
                context: None,
            })
            .await
            .unwrap()
            .into_inner();
        if let Some(srcs) = f.get("source").and_then(|v| v.as_array()) {
            for src in srcs {
                if let Some(d) = src.as_str() {
                    dia2nodes.entry(d.to_string()).or_default().push(resp.node_id);
                }
            }
        }
        *total_turns += 1;
        if *total_turns % 50 == 0 {
            println!("[eval] weave 进度 {total_turns} 条");
        }
    }
}

#[tokio::test]
#[ignore = "需要 LoCoMo 数据集（NYLON_LOCOMO_PATH），手动运行"]
async fn locomo_evidence_recall() {
    let path = std::env::var("NYLON_LOCOMO_PATH").expect("请设置 NYLON_LOCOMO_PATH 指向 locomo10.json");
    let limit: usize = std::env::var("NYLON_LOCOMO_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    // Cat4 ablation：NYLON_CAT4_MAX_HOPS=0 时 Cat4 查询仅返回种子（不扩散）
    // 按类别联想深度：NYLON_CAT{n}_MAX_HOPS 覆盖单类（0=仅种子精准召回），缺省回落 NYLON_MAX_HOPS
    let cat_hops = |cat: i64| -> Option<u32> {
        std::env::var(format!("NYLON_CAT{cat}_MAX_HOPS"))
            .ok()
            .and_then(|v| v.parse().ok())
            .or_else(|| std::env::var("NYLON_MAX_HOPS").ok().and_then(|v| v.parse().ok()))
    };
    let data: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("读取数据集失败")).expect("解析 JSON 失败");

    // 内存端口起服务
    let dir = tempfile::tempdir().unwrap();
    let store = PersistentGraph::open(dir.path()).unwrap();
    let dims: usize = std::env::var("NYLON_EMBED_DIMS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(service::DEFAULT_EMBED_DIMS);
    let embedder = nylon_embed::embedder_from_env(dims);
    let embedder_on = embedder.is_some();
    let llm = llm_from_env();
    let llm_on = llm.is_some();
    let query_expand = std::env::var("NYLON_QUERY_EXPAND").is_ok() && llm_on;
    if embedder_on {
        println!("[eval] 嵌入通道已启用 (NYLON_EMBED_URL), dims={dims}");
    if llm_on {
        println!("[eval] LLM 通道已启用 (NYLON_LLM_URL)，编织分解开启");
    } else {
        println!("[eval] 未配置 NYLON_LLM_URL，走启发式分解");
    }
    } else {
        println!("[eval] 未配置 NYLON_EMBED_URL，走纯词面口径 (dims={dims})");
    if llm_on {
        println!("[eval] LLM 通道已启用 (NYLON_LLM_URL)，编织分解开启");
    } else {
        println!("[eval] 未配置 NYLON_LLM_URL，走启发式分解");
    }
    }
    let expander = llm.clone(); // 查询扩展专用（NYLON_QUERY_EXPAND=1 启用）
    // weave 时 LLM 分解默认关闭（太慢），NYLON_WEAVE_LLM=1 才启用
    let svc_llm = if std::env::var("NYLON_WEAVE_LLM").is_ok() { llm.clone() } else { None };
    // session 级编织：整段 session 一次性给 LLM 分解（指代消解+原子事实+来源标注），NYLON_SESSION_WEAVE=1 启用
    let session_weave = std::env::var("NYLON_SESSION_WEAVE").is_ok() && llm_on;
    if session_weave {
        println!("[eval] session 级编织已启用 (NYLON_SESSION_WEAVE=1)");
    }
    let svc = EngineService::new(store, dims, embedder, svc_llm);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(MemoryEngineServer::new(svc))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    let mut client = MemoryEngineClient::connect(addr).await.unwrap();

    let mut total = 0usize;
    let mut hit = 0usize;
    let mut per_cat: HashMap<i64, (usize, usize, usize)> = HashMap::new(); // cat -> (total, hit, seed_hit)
    let mut seed_total_hit = 0usize;
    let mut total_turns = 0usize;

    for conv in data.as_array().expect("顶层应为数组").iter().take(limit) {
        let sample = conv["sample_id"].as_str().unwrap_or("unknown").to_string();
        let conv_obj = conv["conversation"].as_object().expect("conversation 应为对象");

        // 按 session 数字序织入全部轮次
        let mut sessions: Vec<&String> = conv_obj
            .keys()
            .filter(|k| k.starts_with("session_") && !k.ends_with("_date_time"))
            .collect();
        sessions.sort_by_key(|k| {
            k.trim_start_matches("session_").parse::<u32>().unwrap_or(0)
        });

        let mut dia2nodes: HashMap<String, Vec<u64>> = HashMap::new();
        for sess in sessions {
            let turns = conv_obj[sess].as_array().cloned().unwrap_or_default();
            // 双层写入：叶子层=逐轮原文（保精准回忆）；session_weave 时叠加抽象层=LLM 事实（保推理）
            weave_turns(&mut client, &sample, &turns, &mut dia2nodes, &mut total_turns).await;
            if session_weave {
                weave_session_facts(&mut client, &llm, &sample, &turns, &mut dia2nodes, &mut total_turns).await;
            }
        }

        // 对每个可答 QA 跑共振检索
        for qa in conv["qa"].as_array().cloned().unwrap_or_default() {
            let cat = qa["category"].as_i64().unwrap_or(0);
            if cat == 5 {
                continue; // 对抗题不计入
            }
            let evidence: Vec<String> = qa["evidence"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|e| e.as_str().map(|s| s.to_string()))
                .collect();
            if evidence.is_empty() {
                continue;
            }
            let question = qa["question"].as_str().unwrap_or("");
            let expanded = if query_expand {
                expand_query(expander.as_deref(), question).await.unwrap_or_else(|| question.to_string())
            } else {
                question.to_string()
            };
            let resp = client
                .resonate(ResonateRequest {
                    tenant_id: "locomo".into(),
                    owner_id: sample.clone(),
                    query: expanded.into(),
                    context: cat_hops(cat).map(|h| ContextSpectrum {
                        task: None,
                        emotion_valence: None,
                        device: None,
                        max_hops: Some(h),
                    }),
                    budget: std::env::var("NYLON_BUDGET").ok().and_then(|v| v.parse().ok()).unwrap_or(32),
                })
                .await
                .unwrap()
                .into_inner();
            let got: Vec<u64> = resp.activated.iter().take(RECALL_K).map(|a| a.node_id).collect();
            let ok = evidence
                .iter()
                .any(|e| dia2nodes.get(e).map(|ns| ns.iter().any(|n| got.contains(n))).unwrap_or(false));
            // 种子层召回：证据是否直接进入种子集（不扩散的理论上限）
            let seed_hit = evidence
                .iter()
                .any(|e| dia2nodes.get(e).map(|ns| ns.iter().any(|n| resp.seed_ids.contains(n))).unwrap_or(false));
            total += 1;
            if ok {
                hit += 1;
            }
            if seed_hit {
                seed_total_hit += 1;
            }
            let entry = per_cat.entry(cat).or_insert((0, 0, 0));
            entry.0 += 1;
            if ok {
                entry.1 += 1;
            }
            if seed_hit {
                entry.2 += 1;
            }
        }
    }

    println!();
    println!("=== LoCoMo 子集评测（证据召回 recall@{RECALL_K}, {} 口径） ===", if embedder_on { "词面+向量融合" } else { "纯词面" });
    println!("会话数: {limit}, 织入轮次: {total_turns}");
    for cat in 1..=4i64 {
        if let Ok(v) = std::env::var(format!("NYLON_CAT{cat}_MAX_HOPS")) {
            println!("Cat{cat} ablation active: max_hops={v}");
        }
    }
    if let Ok(v) = std::env::var("NYLON_MAX_HOPS") {
        println!("Global ablation active: max_hops={v}");
    }
    if total > 0 {
        println!("有效 QA: {total}, 命中: {hit}, recall@{RECALL_K} = {:.1}%", hit as f64 / total as f64 * 100.0);
    } else {
        println!("无有效 QA");
    }
    println!("种子层召回: {seed_total_hit}/{total} = {:.1}%", seed_total_hit as f64 / total.max(1) as f64 * 100.0);
    let mut cats: Vec<_> = per_cat.iter().map(|(c, v)| (*c, *v)).collect();
    cats.sort_by_key(|(c, _)| *c);
    for (cat, (t, h, sh)) in &cats {
        println!("  category {cat}: 最终 {h}/{t} = {:.1}% | 种子 {sh}/{t} = {:.1}%",
            *h as f64 / *t as f64 * 100.0, *sh as f64 / *t as f64 * 100.0);
    }
}

/// LLM 查询扩展：把问题改写成 3-6 个实体关键词，附加在原问题后，提升词面/向量种子命中
async fn expand_query(llm: Option<&dyn nylon_llm::ChatModel>, question: &str) -> Option<String> {
    let llm = llm?;
    let system = "You are a search query expander for a conversation memory system. Given a question about past conversations, output ONLY valid JSON: {\"keywords\": [3-6 key entities, names, places, dates, or topics that likely appear verbatim in the original conversation]. Use the original language of the question. No explanations.";
    let v = llm.chat_json(system, question).await.ok()?;
    let kws: Vec<String> = v.get("keywords")?.as_array()?
        .iter().filter_map(|k| k.as_str().map(|s| s.to_string())).collect();
    if kws.is_empty() { return None; }
    Some(format!("{question} {}", kws.join(" ")))
}

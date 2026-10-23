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

#[path = "../src/audit.rs"]
mod audit;

#[path = "../src/auth.rs"]
mod auth;

#[path = "../src/service.rs"]
mod service;

use nylon_llm::{llm_from_env, ChatModel, HttpChatModel};
use nylon_storage::PersistentGraph;
use service::pb::memory_engine_client::MemoryEngineClient;
use service::pb::memory_engine_server::MemoryEngineServer;
use service::pb::*;
use service::EngineService;
use std::collections::HashMap;

const RECALL_K: usize = 10;

/// e2e 作答/裁判专用模型：NYLON_EVAL_QA_MODEL 覆盖作答与裁判所用模型，
/// URL/Key 缺省回落 NYLON_LLM_URL / NYLON_LLM_API_KEY。
/// 动机（2026-09-07）：e2e 与编织共用 deepseek-v4-flash，作答瓶颈掩盖了
/// 检索层的真实水位（recall 86.3% 但 e2e 仅 55.9%）。分离后可用强模型
/// （如 deepseek-v4-pro）作答，测出"检索够强、作答拖后腿"的真实差距。
/// 注意：deepseek-chat / deepseek-reasoner 已是 deepseek-v4-flash 的别名
/// （2026-09-07 实测 /models 与响应回声确认），强模型必须用 deepseek-v4-pro。
/// 推理模型作答保持 thinking 开启（预算 8192，超时 120s），
/// 否则思考链烧光默认 1536 token 导致 JSON 截断、被误判为答错。
/// NYLON_EVAL_QA_TEMPERATURE：数字=显式温度；"omit"=不发送该字段
/// （kimi-k3 只接受 temperature=1，显式发 0 会被 HTTP 400 拒绝）。
fn qa_llm_from_env() -> Option<std::sync::Arc<dyn ChatModel>> {
    let url = std::env::var("NYLON_EVAL_QA_URL")
        .ok()
        .or_else(|| std::env::var("NYLON_LLM_URL").ok())?;
    let model = std::env::var("NYLON_EVAL_QA_MODEL")
        .ok()
        .or_else(|| std::env::var("NYLON_LLM_MODEL").ok())
        .unwrap_or_else(|| "deepseek-v4-flash".into());
    let key = std::env::var("NYLON_EVAL_QA_API_KEY")
        .ok()
        .or_else(|| std::env::var("NYLON_LLM_API_KEY").ok());
    println!("[eval] e2e 作答/裁判模型: {model}");
    let temp = match std::env::var("NYLON_EVAL_QA_TEMPERATURE").ok().as_deref() {
        Some("omit") => None,
        Some(s) => s.parse::<f32>().ok().map(Some).unwrap_or(Some(0.0)),
        None => Some(0.0),
    };
    let m = HttpChatModel::new(url, model, key)
        .with_thinking_off(false)
        .with_max_tokens(8192)
        .with_timeout(120)
        .with_temperature(temp);
    Some(std::sync::Arc::new(m))
}

/// 网络抖动重试：LLM/嵌入服务瞬时不可达时指数退避重试，
/// 避免 1 小时长跑评测因一次网卡掉线全盘作废（2026-09-06 两次踩坑）。
/// 注意：weave_session 中途失败可能已有部分叶子上库，重试会产生少量重复节点，
/// 评测口径下可接受（生产路径不重试，由调用方决定语义）。
async fn rpc_with_retry<F, Fut, T>(what: &str, mut f: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<tonic::Response<T>, tonic::Status>>,
{
    let mut delay = 5u64;
    for attempt in 1..=8u32 {
        match f().await {
            Ok(resp) => return resp.into_inner(),
            Err(e) => {
                if attempt == 8 {
                    panic!("{what} 重试 8 次仍失败: {e:?}");
                }
                eprintln!("[eval] {what} 失败（第 {attempt}/8 次），{delay}s 后重试: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                delay = (delay * 3).min(120);
            }
        }
    }
    unreachable!()
}

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

#[tokio::test]
#[ignore = "需要 LoCoMo 数据集（NYLON_LOCOMO_PATH），手动运行"]
async fn locomo_evidence_recall() {
    let path =
        std::env::var("NYLON_LOCOMO_PATH").expect("请设置 NYLON_LOCOMO_PATH 指向 locomo10.json");
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
            .or_else(|| {
                std::env::var("NYLON_MAX_HOPS")
                    .ok()
                    .and_then(|v| v.parse().ok())
            })
    };
    // 失败解剖：NYLON_EVAL_DUMP_MISS=1 打印未命中题的题目/证据/top-10 实际返回；
    // NYLON_EVAL_DUMP_CAT=3 只看某类（默认全部）。evidence_pos = 证据在完整 budget 内的最早位次。
    let dump_miss = std::env::var("NYLON_EVAL_DUMP_MISS").is_ok();
    let dump_cat: Option<i64> = std::env::var("NYLON_EVAL_DUMP_CAT")
        .ok()
        .and_then(|v| v.parse().ok());
    let data: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("读取数据集失败"))
            .expect("解析 JSON 失败");

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
    // 端到端 QA 口径：NYLON_EVAL_E2E=1 时，除证据召回外，LLM 用 top-10 检索内容作答，
    // 再由裁判 LLM 判定语义正确性（用户真实体验口径，LoCoMo 官方对比口径）
    let e2e = std::env::var("NYLON_EVAL_E2E").is_ok() && llm_on;
    let qa_llm = if e2e { qa_llm_from_env() } else { None };
    let query_expand = std::env::var("NYLON_QUERY_EXPAND").is_ok() && llm_on;
    // 按类别查询扩展（仅评测）：NYLON_CAT{n}_EXPAND=1 时仅对该类别启用 LLM 扩展
    let cat_expand = |cat: i64| -> bool {
        query_expand || (std::env::var(format!("NYLON_CAT{cat}_EXPAND")).is_ok() && llm_on)
    };
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
    // session 级编织（引擎内建双层写入），NYLON_SESSION_WEAVE=1 启用
    let session_weave = std::env::var("NYLON_SESSION_WEAVE").is_ok() && llm_on;
    if session_weave {
        println!("[eval] session 级编织已启用 (NYLON_SESSION_WEAVE=1, 引擎内建双层写入)");
    }
    // 引擎需要 LLM 的场景：逐事件分解（NYLON_WEAVE_LLM）或 session 抽象层（NYLON_SESSION_WEAVE）
    let svc_llm = if std::env::var("NYLON_WEAVE_LLM").is_ok() || session_weave {
        llm.clone()
    } else {
        None
    };
    let expander = llm.clone(); // 查询扩展专用（NYLON_QUERY_EXPAND=1 启用）
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
    // e2e QA 计数：cat -> (total, correct)
    let mut qa_total = 0usize;
    let mut qa_correct = 0usize;
    let mut qa_correct_strict = 0usize;
    let mut qa_per_cat: HashMap<i64, (usize, usize, usize)> = HashMap::new();

    for conv in data.as_array().expect("顶层应为数组").iter().take(limit) {
        let sample = conv["sample_id"].as_str().unwrap_or("unknown").to_string();
        let conv_obj = conv["conversation"]
            .as_object()
            .expect("conversation 应为对象");

        // 按 session 数字序织入全部轮次
        let mut sessions: Vec<&String> = conv_obj
            .keys()
            .filter(|k| k.starts_with("session_") && !k.ends_with("_date_time"))
            .collect();
        sessions.sort_by_key(|k| k.trim_start_matches("session_").parse::<u32>().unwrap_or(0));

        let mut dia2nodes: HashMap<String, Vec<u64>> = HashMap::new();
        // dia_id -> "speaker: text"，供失败解剖 dump 证据原文
        let mut dia2text: HashMap<String, String> = HashMap::new();
        if dump_miss {
            for sess in &sessions {
                for t in conv_obj[sess.as_str()]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                {
                    if let (Some(d), Some(txt)) = (t["dia_id"].as_str(), t["text"].as_str()) {
                        dia2text.insert(
                            d.to_string(),
                            format!("{}: {}", t["speaker"].as_str().unwrap_or(""), txt),
                        );
                    }
                }
            }
        }
        for sess in sessions {
            let turns = conv_obj[sess].as_array().cloned().unwrap_or_default();
            // 时间锚定（NYLON_EVAL_DATE_ANCHOR=1）：叶子文本前挂会话日期。
            // 时序推理题（Cat2）的金答案大多是日期，没有日期上下文根本不可答。
            let date_anchor = if std::env::var("NYLON_EVAL_DATE_ANCHOR").is_ok() {
                conv_obj[format!("{sess}_date_time").as_str()]
                    .as_str()
                    .and_then(|s| s.split(" on ").nth(1).map(|d| d.to_string()))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            if session_weave {
                // 引擎内建双层写入：一次 RPC 完成叶子层+抽象层+层间边
                let events: Vec<SessionEvent> = turns
                    .iter()
                    .filter_map(|t| {
                        let dia = t["dia_id"].as_str().unwrap_or("");
                        let text = t["text"].as_str().unwrap_or("");
                        if dia.is_empty() || text.is_empty() {
                            return None;
                        }
                        Some(SessionEvent {
                            event_id: dia.to_string(),
                            speaker: t["speaker"].as_str().unwrap_or("").to_string(),
                            text: if date_anchor.is_empty() {
                                text.to_string()
                            } else {
                                format!("[{date_anchor}] {text}")
                            },
                        })
                    })
                    .collect();
                if !events.is_empty() {
                    let resp = rpc_with_retry("weave_session", || {
                        let mut c = client.clone();
                        let events = events.clone();
                        let owner = sample.clone();
                        async move {
                            c.weave_session(WeaveSessionRequest {
                                tenant_id: "locomo".into(),
                                owner_id: owner,
                                events,
                                skip_abstract: false,
                            })
                            .await
                        }
                    })
                    .await;
                    for en in &resp.leaf_nodes {
                        if !en.event_id.is_empty() {
                            dia2nodes
                                .entry(en.event_id.clone())
                                .or_default()
                                .push(en.node_id);
                        }
                    }
                    for f in &resp.fact_nodes {
                        for sid in &f.source_event_ids {
                            dia2nodes.entry(sid.clone()).or_default().push(f.node_id);
                        }
                    }
                    total_turns += resp.leaf_nodes.len() + resp.fact_nodes.len();
                    println!(
                        "[eval] {} 双层写入: {} 叶子 + {} 事实 (累计 {})",
                        sess,
                        resp.leaf_nodes.len(),
                        resp.fact_nodes.len(),
                        total_turns
                    );
                }
                continue;
            }
            weave_turns(
                &mut client,
                &sample,
                &turns,
                &mut dia2nodes,
                &mut total_turns,
            )
            .await;
        }

        if session_weave && std::env::var("NYLON_WORLD_BRIDGES_ASYNC").is_ok() {
            let wait_secs = std::env::var("NYLON_REFLECT_WAIT_SECS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(45);
            println!("[eval] 等待异步反思 {wait_secs}s 后进入查询");
            tokio::time::sleep(std::time::Duration::from_secs(wait_secs)).await;
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
            let expanded = if cat_expand(cat) {
                expand_query(expander.as_deref(), question, cat)
                    .await
                    .unwrap_or_else(|| question.to_string())
            } else {
                question.to_string()
            };
            // 按类别实验旋钮（仅评测）：NYLON_CAT{n}_SEEDS / NYLON_CAT{n}_RERANK 临时覆盖全局值
            let saved_seeds = std::env::var("NYLON_MAX_SEEDS").ok();
            let saved_rerank = std::env::var("NYLON_RERANK_VEC").ok();
            if let Ok(v) = std::env::var(format!("NYLON_CAT{cat}_SEEDS")) {
                std::env::set_var("NYLON_MAX_SEEDS", &v);
            }
            if let Ok(v) = std::env::var(format!("NYLON_CAT{cat}_RERANK")) {
                std::env::set_var("NYLON_RERANK_VEC", &v);
            }
            let resp = rpc_with_retry("resonate", || {
                let mut c = client.clone();
                let owner = sample.clone();
                let query = expanded.clone();
                async move {
                    c.resonate(ResonateRequest {
                        tenant_id: "locomo".into(),
                        owner_id: owner,
                        query,
                        context: cat_hops(cat).map(|h| ContextSpectrum {
                            task: None,
                            emotion_valence: None,
                            device: None,
                            max_hops: Some(h),
                        }),
                        budget: std::env::var("NYLON_BUDGET")
                            .ok()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(32),
                    })
                    .await
                }
            })
            .await;
            match &saved_seeds {
                Some(v) => std::env::set_var("NYLON_MAX_SEEDS", v),
                None => std::env::remove_var("NYLON_MAX_SEEDS"),
            }
            match &saved_rerank {
                Some(v) => std::env::set_var("NYLON_RERANK_VEC", v),
                None => std::env::remove_var("NYLON_RERANK_VEC"),
            }
            let got: Vec<u64> = resp
                .activated
                .iter()
                .take(RECALL_K)
                .map(|a| a.node_id)
                .collect();
            let ok = evidence.iter().any(|e| {
                dia2nodes
                    .get(e)
                    .map(|ns| ns.iter().any(|n| got.contains(n)))
                    .unwrap_or(false)
            });
            // 种子层召回：证据是否直接进入种子集（不扩散的理论上限）
            let seed_hit = evidence.iter().any(|e| {
                dia2nodes
                    .get(e)
                    .map(|ns| ns.iter().any(|n| resp.seed_ids.contains(n)))
                    .unwrap_or(false)
            });
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
            // e2e：top-10 检索内容 → LLM 作答 → 裁判判定语义正确性
            if e2e {
                let gold = qa["answer"].as_str().unwrap_or("");
                if gold.trim().is_empty() {
                    continue; // 数据集中少数条目无金答案，无法判定，不计入
                }
                let ctx_text = resp
                    .activated
                    .iter()
                    // 机制验证（NYLON_EVAL_CAT2_NO_PERSONA=1）：时序题作答上下文剔除画像节点。
                    // 画像是跨时间聚合文本、无日期锚点，实测会把时序题答案带偏
                    // （第二轮 A/B：Cat2 错题 7/8 召回命中但答错）。先过滤再取 Top-K，
                    // 让被剔除的画像名额由后续证据补位；recall@10 统计不受影响（用上文未过滤的 got）。
                    .filter(|a| {
                        !(cat == 2
                            && std::env::var("NYLON_EVAL_CAT2_NO_PERSONA").is_ok()
                            && a.filaments
                                .as_ref()
                                .is_some_and(|f| f.relations.iter().any(|r| r == "persona")))
                    })
                    .take(RECALL_K)
                    .filter_map(|a| a.filaments.as_ref().map(|f| f.fact.clone()))
                    .collect::<Vec<_>>()
                    .join("\n");
                let candidate = answer_with_context(qa_llm.as_deref(), &ctx_text, question).await;
                // 双裁判：论文口径（Mem0 Appendix A，从宽，对外可比）+ 内部严格口径（从严，看真实质量）
                let (correct, correct_strict) = match &candidate {
                    Some(ans) => {
                        let p = judge_answer_paper(qa_llm.as_deref(), question, gold, ans)
                            .await
                            .unwrap_or(false);
                        let s = judge_answer_strict(qa_llm.as_deref(), question, gold, ans)
                            .await
                            .unwrap_or(false);
                        (p, s)
                    }
                    None => (false, false),
                };
                qa_total += 1;
                if correct {
                    qa_correct += 1;
                }
                if correct_strict {
                    qa_correct_strict += 1;
                }
                let e = qa_per_cat.entry(cat).or_insert((0, 0, 0));
                e.0 += 1;
                if correct {
                    e.1 += 1;
                }
                if correct_strict {
                    e.2 += 1;
                }
                if dump_miss && !correct && dump_cat.map(|c| c == cat).unwrap_or(true) {
                    println!("\n[QA-WRONG] sample={sample} cat={cat} recall_hit={ok}");
                    println!("  Q: {question}");
                    println!("  gold: {gold}");
                    println!("  ours: {}", candidate.as_deref().unwrap_or("<无答案>"));
                }
            }
            if dump_miss && !ok && dump_cat.map(|c| c == cat).unwrap_or(true) {
                // 证据在完整 budget（默认 32）内的最早位次：None=完全没召回，11+=排序问题
                let evidence_pos = evidence
                    .iter()
                    .filter_map(|e| dia2nodes.get(e))
                    .flatten()
                    .filter_map(|n| {
                        resp.activated
                            .iter()
                            .position(|a| a.node_id == *n)
                            .map(|p| p + 1)
                    })
                    .min();
                println!("\n[MISS] sample={sample} cat={cat} seed_hit={seed_hit} evidence_pos={evidence_pos:?}");
                println!("  Q: {question}");
                for e in &evidence {
                    let txt = dia2text.get(e).map(|s| s.as_str()).unwrap_or("");
                    let snip: String = txt.chars().take(100).collect();
                    println!("  E[{e}]: {snip}");
                }
                // 解剖需要看 recall@10 之外的位次（画像/桥节点是否"差一点"），打印 top-15
                for (i, a) in resp.activated.iter().take(15).enumerate() {
                    let fact = a.filaments.as_ref().map(|f| f.fact.as_str()).unwrap_or("");
                    let snip: String = fact.chars().take(90).collect();
                    println!(
                        "  {:>2}. n{} r={:.3} {}",
                        i + 1,
                        a.node_id,
                        a.resonance,
                        snip
                    );
                }
            }
        }
    }

    println!();
    println!(
        "=== LoCoMo 子集评测（证据召回 recall@{RECALL_K}, {} 口径） ===",
        if embedder_on {
            "词面+向量融合"
        } else {
            "纯词面"
        }
    );
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
        println!(
            "有效 QA: {total}, 命中: {hit}, recall@{RECALL_K} = {:.1}%",
            hit as f64 / total as f64 * 100.0
        );
    } else {
        println!("无有效 QA");
    }
    println!(
        "种子层召回: {seed_total_hit}/{total} = {:.1}%",
        seed_total_hit as f64 / total.max(1) as f64 * 100.0
    );
    let mut cats: Vec<_> = per_cat.iter().map(|(c, v)| (*c, *v)).collect();
    cats.sort_by_key(|(c, _)| *c);
    for (cat, (t, h, sh)) in &cats {
        println!(
            "  category {cat}: 最终 {h}/{t} = {:.1}% | 种子 {sh}/{t} = {:.1}%",
            *h as f64 / *t as f64 * 100.0,
            *sh as f64 / *t as f64 * 100.0
        );
    }
    if e2e && qa_total > 0 {
        println!();
        println!("=== 端到端 QA 准确率（top-{RECALL_K} 检索 → LLM 作答 → 双裁判判定） ===");
        println!(
            "有效 QA: {qa_total}, 论文口径(J): {qa_correct} = {:.1}% | 严格口径: {qa_correct_strict} = {:.1}%",
            qa_correct as f64 / qa_total as f64 * 100.0,
            qa_correct_strict as f64 / qa_total as f64 * 100.0
        );
        let mut qcats: Vec<_> = qa_per_cat.iter().map(|(c, v)| (*c, *v)).collect();
        qcats.sort_by_key(|(c, _)| *c);
        for (cat, (t, c, cs)) in &qcats {
            println!(
                "  category {cat}: J {c}/{t} = {:.1}% | 严格 {cs}/{t} = {:.1}%",
                *c as f64 / *t as f64 * 100.0,
                *cs as f64 / *t as f64 * 100.0
            );
        }
    }
}

/// LLM 查询扩展：普通类目扩关键词；Cat3 可选 HyDE 生成假设证据句。
/// e2e LLM 调用重试：网络抖动/限流时退避重试，避免单点失败污染准确率。
async fn llm_json_retry(
    llm: &dyn nylon_llm::ChatModel,
    system: &str,
    user: &str,
) -> Option<serde_json::Value> {
    let mut delay = 5u64;
    for attempt in 1..=4u32 {
        match llm.chat_json(system, user).await {
            Ok(v) => return Some(v),
            Err(e) => {
                if attempt == 4 {
                    eprintln!("[eval] e2e LLM 调用重试 4 次仍失败: {e}");
                    return None;
                }
                eprintln!("[eval] e2e LLM 调用失败（第 {attempt}/4 次），{delay}s 后重试: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                delay = (delay * 2).min(60);
            }
        }
    }
    None
}

/// 作答器（对齐 Mem0 论文生成模板，Appendix A "Prompt Template for Results Generation"）：
/// 仅用检索记忆作答；时间相对引用按记忆时间戳换算绝对日期；矛盾取最新；
/// 答案尽量简短（宽松裁判配套）；信息不足必须答 "Not mentioned"。
async fn answer_with_context(
    llm: Option<&dyn nylon_llm::ChatModel>,
    ctx: &str,
    question: &str,
) -> Option<String> {
    let llm = llm?;
    let system = "You are an intelligent memory assistant tasked with retrieving accurate information from conversation memories. \
        Instructions: \
        1. Carefully analyze all provided memories; each memory may be prefixed with a timestamp like [8 May, 2023], pay special attention to these timestamps. \
        2. If the memories contain contradictory information, prioritize the most recent memory. \
        3. For relative time references (like \"last year\" or \"two months ago\"), calculate the specific date, month, or year based on the memory timestamps. \
        4. Formulate a precise, concise answer based solely on the evidence in the memories: a short phrase for factual questions, or the minimal list of items for listing questions. \
        5. If the memories do not contain enough information, the answer must be exactly \"Not mentioned\". \
        Output ONLY valid JSON: {\"answer\": \"...\"}.";
    let user = format!("Retrieved memories:\n{ctx}\n\nQuestion: {question}");
    let v = llm_json_retry(llm, system, &user).await?;
    v.get("answer")?
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 裁判·论文口径（Mem0 论文 Appendix A 几乎逐字，binary CORRECT/WRONG，从宽）：
/// "触及同一话题即 CORRECT"、时间题宽松。J 分 = CORRECT 占比（论文跑 10 次取均值，我们跑 1 次）。
async fn judge_answer_paper(
    llm: Option<&dyn nylon_llm::ChatModel>,
    question: &str,
    gold: &str,
    candidate: &str,
) -> Option<bool> {
    let llm = llm?;
    let system = "Your task is to label an answer to a question as \"CORRECT\" or \"WRONG\". You will be given the following data: (1) a question (posed by one user to another user), (2) a 'gold' (ground truth) answer, (3) a generated answer which you will score as CORRECT/WRONG. \
        The point of the question is to ask about something one user should know about the other user based on their prior conversations. The gold answer will usually be a concise and short answer that includes the referenced topic. \
        The generated answer might be much longer, but you should be generous with your grading - as long as it touches on the same topic as the gold answer, it should be counted as CORRECT. \
        For time related questions, the gold answer will be a specific date, month, year, etc. The generated answer might be much longer or use relative time references (like 'last Tuesday' or 'next month'), but you should be generous with your grading - as long as it refers to the same date or time period as the gold answer, it should be counted as CORRECT. Even if the format differs (e.g., 'May 7th' vs '7 May'), consider it CORRECT if it's the same date. \
        Return ONLY valid JSON with the label: {\"label\": \"CORRECT\"} or {\"label\": \"WRONG\"}.";
    let user = format!("Question: {question}\nGold answer: {gold}\nGenerated answer: {candidate}");
    llm_json_retry(llm, system, &user)
        .await?
        .get("label")?
        .as_str()
        .map(|s| s.trim().eq_ignore_ascii_case("CORRECT"))
}

/// 裁判·内部严格口径（原 judge_answer）：语义等价判定，用于观察真实作答质量，
/// 与论文宽松口径并列报告，防止从宽裁判掩盖半对答案。
async fn judge_answer_strict(
    llm: Option<&dyn nylon_llm::ChatModel>,
    question: &str,
    gold: &str,
    candidate: &str,
) -> Option<bool> {
    let llm = llm?;
    let system = "You are a strict but fair evaluation judge. Given a question, a reference answer, and a candidate answer, decide if the candidate conveys the same substantive answer. Wording may differ; reasonable inference grounded in the reference is acceptable; approximate dates/numbers are acceptable if close. If the candidate says the information is not mentioned but the reference exists, it is wrong. Output ONLY valid JSON: {\"correct\": true} or {\"correct\": false}.";
    let user =
        format!("Question: {question}\nReference answer: {gold}\nCandidate answer: {candidate}");
    llm_json_retry(llm, system, &user)
        .await?
        .get("correct")?
        .as_bool()
}

async fn expand_query(
    llm: Option<&dyn nylon_llm::ChatModel>,
    question: &str,
    cat: i64,
) -> Option<String> {
    let llm = llm?;
    if cat == 3 && std::env::var("NYLON_CAT3_HYDE").is_ok() {
        let system = "You are a hypothesis document expander for a conversation memory system. Given a question that may require inference over past conversations, write one concise hypothetical evidence passage (1-2 sentences, at most 80 words) that would directly answer or justify the question. Do not use hedging words; include concrete entities, numbers, or dates only when clearly implied. Output ONLY valid JSON: {\"passage\": \"your passage here\"}. Use the original language of the question. No explanations.";
        let v = llm.chat_json(system, question).await.ok()?;
        let passage = v.get("passage").and_then(|p| p.as_str()).map(str::trim)?;
        if passage.is_empty() {
            return None;
        }
        return Some(format!("{question} {passage}"));
    }
    let system = if cat == 3 && std::env::var("NYLON_CAT3_EXPAND_V2").is_ok() {
        "You are a commonsense query expander for a conversation memory system. Given a question that may require inference over past conversations, output ONLY valid JSON: {\"keywords\": [4-8 search terms]. Include explicit entities, the likely answer type, abstract concepts, related event descriptions, and synonyms/paraphrases that may appear in the original conversation. Use the original language of the question. No explanations."
    } else {
        "You are a search query expander for a conversation memory system. Given a question about past conversations, output ONLY valid JSON: {\"keywords\": [3-6 key entities, names, places, dates, or topics that likely appear verbatim in the original conversation]. Use the original language of the question. No explanations."
    };
    let v = llm.chat_json(system, question).await.ok()?;
    let kws: Vec<String> = v
        .get("keywords")?
        .as_array()?
        .iter()
        .filter_map(|k| k.as_str().map(|s| s.to_string()))
        .collect();
    if kws.is_empty() {
        return None;
    }
    Some(format!("{question} {}", kws.join(" ")))
}

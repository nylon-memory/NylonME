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
    // 推断附赠通道（NYLON_EVAL_INFER_BONUS=N，仅评测）：作答上下文 = 前 10 条**非推断**
    // 节点 + 末尾追加最多 N 条推断节点（前缀 [inferred] 供作答模型校准）。
    // 推断不占证据名额（recall-J 背离的解法：57.6 vs 63.0 的挤出不该发生）。
    // recall@10 统计口径不变（got 仍取原始前 10）。
    let infer_bonus: usize = std::env::var("NYLON_EVAL_INFER_BONUS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    // 作答预算加宽（NYLON_EVAL_CTX_K=15/20）：LongMemEval round-4 已验证（J 78.0→83.0，
    // +5pp，配对 +8/-3）。证据在激活池内但 Top-10 装不下时直接受益；
    // 只扩大作答上下文条数，recall@10 统计口径不变。
    let ctx_k: usize = std::env::var("NYLON_EVAL_CTX_K")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(RECALL_K);
    // 分题型自适应宽度（NYLON_EVAL_CTX_K_CAT1/2/3/4，缺省回落 CTX_K）：
    // CTX_K=15 全量显示 Cat3 回退 65.2→60.9（开放域题被宽上下文的干扰项带偏，
    // 翻转 +1/-5），其余类全涨。按题型路由宽度让每类用自己的最优口径。
    let ctx_k_for = |cat: i64| -> usize {
        std::env::var(format!("NYLON_EVAL_CTX_K_CAT{}", cat))
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(ctx_k)
    };
    let data: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("读取数据集失败"))
            .expect("解析 JSON 失败");

    // 内存端口起服务
    // 编织结果缓存（NYLON_EVAL_STORE_DIR）：查询侧实验（PRF/重排/扩展）复用同一份
    // 编织库，跳过约 1 小时的织入阶段，迭代从小时级降到分钟级。
    // 注意：编织侧配置变更（SESSION_WEAVE/SKIP_ABSTRACT/BRIDGES/DATE_ANCHOR/嵌入模型）
    // 必须换用新的缓存目录，否则混库。
    let cache_dir = std::env::var("NYLON_EVAL_STORE_DIR").ok();
    let (_tmpdir, store_path) = match &cache_dir {
        Some(d) => {
            std::fs::create_dir_all(d).expect("创建缓存目录失败");
            (None, std::path::PathBuf::from(d))
        }
        None => {
            let t = tempfile::tempdir().unwrap();
            let p = t.path().to_path_buf();
            (Some(t), p)
        }
    };
    let cache_file = cache_dir
        .as_ref()
        .map(|d| std::path::Path::new(d).join("weave_map.json"));
    let cache_ready = cache_file.as_ref().map(|f| f.exists()).unwrap_or(false);
    let mut cache_map: HashMap<String, HashMap<String, Vec<u64>>> = cache_file
        .as_ref()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if cache_ready {
        println!(
            "[eval] 命中编织缓存 (NYLON_EVAL_STORE_DIR, {} 个会话样本)，逐样本复用",
            cache_map.len()
        );
    }
    let store = PersistentGraph::open(&store_path).unwrap();
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
    let mut per_cat: HashMap<i64, (usize, usize, usize, usize)> = HashMap::new(); // cat -> (total, hit, seed_hit, all_hit)
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
        // 缓存按样本粒度判断：部分缓存（如前一次只跑了 LIMIT=1）只复用已织样本，
        // 未命中样本照常织入并增量落盘
        let sample_cached = cache_map.contains_key(&sample);
        for sess in sessions {
            if sample_cached {
                break;
            }
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
                    let skip_abstract = std::env::var("NYLON_EVAL_SKIP_ABSTRACT").is_ok(); // A4 消融：仅叶层
                    let resp = rpc_with_retry("weave_session", || {
                        let mut c = client.clone();
                        let events = events.clone();
                        let owner = sample.clone();
                        async move {
                            c.weave_session(WeaveSessionRequest {
                                tenant_id: "locomo".into(),
                                owner_id: owner,
                                events,
                                skip_abstract: skip_abstract,
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

        if sample_cached {
            dia2nodes = cache_map.get(&sample).cloned().unwrap_or_default();
            println!(
                "[eval] {sample} 复用缓存编织：{} 个 dia 映射",
                dia2nodes.len()
            );
        } else if let Some(f) = &cache_file {
            cache_map.insert(sample.clone(), dia2nodes.clone());
            std::fs::write(f, serde_json::to_string(&cache_map).unwrap()).expect("写编织缓存失败");
        }

        if !sample_cached && session_weave && std::env::var("NYLON_WORLD_BRIDGES_ASYNC").is_ok() {
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
            // 仅评某类（消融用）：NYLON_EVAL_ONLY_CAT=3 时跳过其他类别
            if let Ok(only) = std::env::var("NYLON_EVAL_ONLY_CAT") {
                if only.parse::<i64>().map(|o| cat != o).unwrap_or(false) {
                    continue;
                }
            }
            // 证据 ID 兼容：少数条目把多个 dia_id 用分号挤在一个字符串里
            // （如 "D8:6; D9:17"），不拆开会导致映射查找永远落空（2026-09-18 发现）。
            let evidence: Vec<String> = qa["evidence"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|e| e.as_str())
                .flat_map(|s| s.split(';'))
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
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
                        top_k: 0,
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
            // 伪相关反馈第二轮检索（NYLON_CAT{n}_PRF=1）：多跳题的第一跳往往
            // 只带回"半条证据链"，用首轮 Top-5 事实反哺查询再检一轮，按最高分合并，
            // 让第二跳证据进入候选。查询侧机制，不改编织。
            let resp = if std::env::var(format!("NYLON_CAT{cat}_PRF")).is_ok() {
                let fb: String = resp
                    .activated
                    .iter()
                    .take(5)
                    .filter_map(|a| a.filaments.as_ref().map(|f| f.fact.clone()))
                    .map(|f| f.chars().take(200).collect::<String>())
                    .collect::<Vec<_>>()
                    .join(" | ");
                if fb.is_empty() {
                    resp
                } else {
                    let q2 = format!("{expanded} {fb}");
                    let resp2 = rpc_with_retry("resonate-prf", || {
                        let mut c = client.clone();
                        let owner = sample.clone();
                        let query = q2.clone();
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
                                top_k: 0,
                            })
                            .await
                        }
                    })
                    .await;
                    // 合并两轮：同一节点取最高分，按分降序重排
                    let mut best: HashMap<u64, ActivatedNode> = HashMap::new();
                    for a in resp
                        .activated
                        .into_iter()
                        .chain(resp2.activated.into_iter())
                    {
                        best.entry(a.node_id)
                            .and_modify(|old| {
                                if a.resonance > old.resonance {
                                    old.resonance = a.resonance;
                                }
                            })
                            .or_insert(a);
                    }
                    let mut merged: Vec<ActivatedNode> = best.into_values().collect();
                    merged.sort_by(|a, b| b.resonance.total_cmp(&a.resonance));
                    ResonateResponse {
                        activated: merged,
                        seed_ids: resp.seed_ids,
                    }
                }
            } else {
                resp
            };
            // 多通道补充检索 + RRF 融合：
            //   NYLON_CAT{n}_DECOMPOSE=1 —— LLM 拆原子子查询（多跳题各跳各找）
            //   NYLON_CAT{n}_ENTITY=1    —— 实体名单独检索（聚合题补上下文广度）
            let extra_queries: Vec<String> = {
                let mut qs: Vec<String> = Vec::new();
                if std::env::var(format!("NYLON_CAT{cat}_DECOMPOSE")).is_ok() {
                    qs.extend(
                        decompose_query(expander.as_deref(), question)
                            .await
                            .into_iter()
                            .filter(|s| s != question),
                    );
                }
                if std::env::var(format!("NYLON_CAT{cat}_ENTITY")).is_ok() {
                    qs.extend(extract_entities(question));
                }
                qs
            };
            let resp = if extra_queries.is_empty() {
                resp
            } else {
                let mut lists: Vec<Vec<ActivatedNode>> = vec![resp.activated.clone()];
                for sq in &extra_queries {
                    let r = rpc_with_retry("resonate-sub", || {
                        let mut c = client.clone();
                        let owner = sample.clone();
                        let query = sq.clone();
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
                                top_k: 0,
                            })
                            .await
                        }
                    })
                    .await;
                    lists.push(r.activated);
                }
                // RRF：score = Σ w/(60 + rank)，主查询权重 1.0，补充通道权重
                // NYLON_RRF_EXTRA_W（默认 0.3）——补充通道只填主查询的空档，
                // 等权会让泛化实体结果与精准命中平分 Top-10（R6 实测 -27.6pp）
                let extra_w: f32 = std::env::var("NYLON_RRF_EXTRA_W")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0.3);
                let mut rrf: HashMap<u64, (f32, ActivatedNode)> = HashMap::new();
                for (li, list) in lists.iter().enumerate() {
                    let w = if li == 0 { 1.0f32 } else { extra_w };
                    for (rank, a) in list.iter().enumerate() {
                        let s = w / (61.0 + rank as f32);
                        rrf.entry(a.node_id)
                            .and_modify(|(score, _)| *score += s)
                            .or_insert_with(|| (s, a.clone()));
                    }
                }
                let mut merged: Vec<(f32, ActivatedNode)> = rrf.into_values().collect();
                merged.sort_by(|a, b| b.0.total_cmp(&a.0));
                ResonateResponse {
                    activated: merged.into_iter().map(|(_, a)| a).collect(),
                    seed_ids: resp.seed_ids,
                }
            };
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
            // 全证据命中：多跳题需要"每一跳"都在 Top-K 才可答，
            // any-hit 会高估多跳题的可答性（2026-09-18 提分专项新增观测口径）
            let all_hit = !evidence.is_empty()
                && evidence.iter().all(|e| {
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
            let entry = per_cat.entry(cat).or_insert((0, 0, 0, 0));
            entry.0 += 1;
            if ok {
                entry.1 += 1;
            }
            if seed_hit {
                entry.2 += 1;
            }
            if all_hit {
                entry.3 += 1;
            }
            // e2e：top-10 检索内容 → LLM 作答 → 裁判判定语义正确性
            if e2e {
                let gold = qa["answer"].as_str().unwrap_or("");
                if gold.trim().is_empty() {
                    continue; // 数据集中少数条目无金答案，无法判定，不计入
                }
                let is_inferred = |a: &ActivatedNode| {
                    a.filaments
                        .as_ref()
                        .is_some_and(|f| f.relations.iter().any(|r| r == "inferred"))
                };
                let mut ctx_items: Vec<String> = resp
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
                    // 附赠通道开启时推断不占 Top-10 证据名额，名额由后续证据补位
                    .filter(|a| !(infer_bonus > 0 && is_inferred(a)))
                    .take(ctx_k_for(cat))
                    .filter_map(|a| a.filaments.as_ref().map(|f| f.fact.clone()))
                    .collect();
                if infer_bonus > 0 {
                    ctx_items.extend(
                        resp.activated
                            .iter()
                            .filter(|a| is_inferred(a))
                            .take(infer_bonus)
                            .filter_map(|a| {
                                a.filaments
                                    .as_ref()
                                    .map(|f| format!("[inferred] {}", f.fact))
                            }),
                    );
                }
                let ctx_text = ctx_items.join("\n");
                let candidate = answer_with_context(qa_llm.as_deref(), &ctx_text, question).await;
                // 双裁判：论文口径（Mem0 Appendix A，从宽，对外可比）+ 内部严格口径（从严，看真实质量）
                let (correct, correct_strict) = match &candidate {
                    Some(ans) => {
                        let p = judge_answer_paper(qa_llm.as_deref(), question, gold, ans)
                            .await
                            .unwrap_or(false);
                        // NYLON_EVAL_SINGLE_JUDGE=1：跳过严格裁判（Kimi 5h 窗口配额减半，
                        // 与 LongMemEval harness 2026-09-30 同款旋钮）
                        let s = if std::env::var("NYLON_EVAL_SINGLE_JUDGE").is_ok() {
                            p
                        } else {
                            judge_answer_strict(qa_llm.as_deref(), question, gold, ans)
                                .await
                                .unwrap_or(false)
                        };
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
                    // 深度解剖（NYLON_EVAL_DUMP_CTX=1）：答错题的全证据命中状态、
                    // 每条证据在完整激活集内的位次、以及实际喂给作答 LLM 的 Top-10 上下文。
                    // 用于区分"证据齐但模型弃答/答错"（prompt 问题）与"any-hit 但缺关键跳"
                    // （排序/检索问题）。
                    if std::env::var("NYLON_EVAL_DUMP_CTX").is_ok() {
                        println!("  all_hit={all_hit}");
                        for e in &evidence {
                            let pos = dia2nodes.get(e).and_then(|ns| {
                                ns.iter()
                                    .filter_map(|n| {
                                        resp.activated
                                            .iter()
                                            .position(|a| a.node_id == *n)
                                            .map(|p| p + 1)
                                    })
                                    .min()
                            });
                            let woven = dia2nodes.contains_key(e);
                            println!("  Epos[{e}]: woven={woven} pos={pos:?}");
                        }
                        println!("  ctx:");
                        for (i, c) in ctx_items.iter().enumerate() {
                            let snip: String = c.chars().take(110).collect();
                            println!("   {:>2}. {snip}", i + 1);
                        }
                    }
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
            // 全证据缺口解剖（NYLON_EVAL_DUMP_ALLHIT=1）：any-hit 但非全命中时，
            // 逐条证据打印其在完整激活集里的最佳位次——11-32 位=排序问题，缺位=检索问题
            if std::env::var("NYLON_EVAL_DUMP_ALLHIT").is_ok()
                && ok
                && !all_hit
                && dump_cat.map(|c| c == cat).unwrap_or(true)
            {
                println!("\n[HOP-MISS] sample={sample} cat={cat}");
                println!("  Q: {question}");
                for e in &evidence {
                    let pos = dia2nodes.get(e).and_then(|ns| {
                        ns.iter()
                            .filter_map(|n| {
                                resp.activated
                                    .iter()
                                    .position(|a| a.node_id == *n)
                                    .map(|p| p + 1)
                            })
                            .min()
                    });
                    let in_seed = dia2nodes
                        .get(e)
                        .map(|ns| ns.iter().any(|n| resp.seed_ids.contains(n)))
                        .unwrap_or(false);
                    let txt = dia2text.get(e).map(|s| s.as_str()).unwrap_or("");
                    let snip: String = txt.chars().take(80).collect();
                    match pos {
                        Some(p) => println!("  E[{e}] pos={p} seed={in_seed} :: {snip}"),
                        None => println!("  E[{e}] pos=ABSENT seed={in_seed} :: {snip}"),
                    }
                }
            }
            // 推断节点解剖（NYLON_EVAL_DUMP_INFER=1）：打印 top-10 中推断节点的位次/原文，
            // 验证"个人化推断是否真正进入作答上下文"——结构性修复的有效性探针。
            if std::env::var("NYLON_EVAL_DUMP_INFER").is_ok()
                && dump_cat.map(|c| c == cat).unwrap_or(true)
            {
                for (i, a) in resp.activated.iter().take(10).enumerate() {
                    let is_infer = a
                        .filaments
                        .as_ref()
                        .map(|f| f.relations.iter().any(|r| r == "inferred"))
                        .unwrap_or(false);
                    if is_infer {
                        let fact = a.filaments.as_ref().map(|f| f.fact.as_str()).unwrap_or("");
                        let snip: String = fact.chars().take(120).collect();
                        println!(
                            "[INFER] sample={sample} cat={cat} rank={} n{} r={:.3} :: {snip}",
                            i + 1,
                            a.node_id,
                            a.resonance
                        );
                    }
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
    for (cat, (t, h, sh, ah)) in &cats {
        println!(
            "  category {cat}: 最终 {h}/{t} = {:.1}% | 种子 {sh}/{t} = {:.1}% | 全证据 {ah}/{t} = {:.1}%",
            *h as f64 / *t as f64 * 100.0,
            *sh as f64 / *t as f64 * 100.0,
            *ah as f64 / *t as f64 * 100.0
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
    // NYLON_EVAL_QA_PROMPT_V2=1：反弃答 + 具体化作答提示。
    // 动机（2026-09-23 miss 解剖）：361 道 J 错题中 163 道弃答，其中 57 道
    // 全证据已在 Top-10 内仍答 "Not mentioned"；另有 80 道内容错但证据齐全
    // （答案笼统/张冠李戴）。弃答在 J 口径下必错，基于部分证据的合理猜测
    // 期望收益为正。
    let v2 = std::env::var("NYLON_EVAL_QA_PROMPT_V2").is_ok();
    let system = if v2 {
        "You are an intelligent memory assistant tasked with retrieving accurate information from conversation memories. \
        Instructions: \
        1. Carefully analyze all provided memories; each memory may be prefixed with a timestamp like [8 May, 2023], pay special attention to these timestamps. \
        2. If the memories contain contradictory information, prioritize the most recent memory. \
        3. For relative time references (like \"last year\" or \"two months ago\"), calculate the specific date, month, or year based on the memory timestamps. \
        4. Formulate a precise, concise answer based solely on the evidence in the memories. Prefer concrete details (names, numbers, dates, specific objects) over generic summaries. For questions asking what/which items, enumerate every relevant item mentioned in the memories. \
        5. Answer \"Not mentioned\" ONLY if none of the memories contain any information relevant to the question. If there is partial or indirect evidence, give your best grounded answer instead of abstaining. \
        Output ONLY valid JSON: {\"answer\": \"...\"}."
    } else {
        "You are an intelligent memory assistant tasked with retrieving accurate information from conversation memories. \
        Instructions: \
        1. Carefully analyze all provided memories; each memory may be prefixed with a timestamp like [8 May, 2023], pay special attention to these timestamps. \
        2. If the memories contain contradictory information, prioritize the most recent memory. \
        3. For relative time references (like \"last year\" or \"two months ago\"), calculate the specific date, month, or year based on the memory timestamps. \
        4. Formulate a precise, concise answer based solely on the evidence in the memories: a short phrase for factual questions, or the minimal list of items for listing questions. \
        5. If the memories do not contain enough information, the answer must be exactly \"Not mentioned\". \
        Output ONLY valid JSON: {\"answer\": \"...\"}."
    };
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

/// 多跳问题分解（NYLON_CAT{n}_DECOMPOSE=1）：LLM 把复杂问题拆成 1-3 个原子子查询，
/// 每个子查询各跑一次共振，按 RRF（reciprocal rank fusion）融合。
/// 动机（2026-09-18）：Cat1 全证据命中仅 27.3%——单查询往往只找回第一跳；
/// PRF 伪相关反馈实测 -7.8pp（反馈放大第一跳簇，挤掉第二跳），
/// 分解让不同子查询各找一跳，RRF 防止单一查询的簇主导。
async fn decompose_query(llm: Option<&dyn nylon_llm::ChatModel>, question: &str) -> Vec<String> {
    let Some(llm) = llm else {
        return Vec::new();
    };
    let system = "You decompose complex questions about past conversations into atomic search sub-queries. \
        Output ONLY valid JSON: {\"sub\": [\"...\", ...]} with 1-3 sub-queries that together cover the question. \
        Each sub-query should target one specific fact, event, or time point mentioned or implied by the question. \
        If the question is already atomic, return a single-element list with the original question. \
        Use the original language of the question. No explanations.";
    let v = match llm.chat_json(system, question).await {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    v.get("sub")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::trim))
                .filter(|x| !x.is_empty())
                .map(|x| x.to_string())
                .take(3)
                .collect()
        })
        .unwrap_or_default()
}

/// 聚合题实体补充检索（NYLON_CAT{n}_ENTITY=1）：从问题里抽大写开头的人名/实体，
/// 以实体名单独跑共振，与主查询结果 RRF 融合。
/// 动机（2026-09-18）：Cat1 全证据缺口中 3/4 的缺位跳完全不在激活池内，
/// 且多为聚合题（"What activities does Melanie partake in?"）——证据轮与问题
/// 词面/语义重叠极低，单查询无法触达；实体名查询种子覆盖该人的全部事实，
/// 恰好补上聚合所需的上下文广度。
fn extract_entities(question: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "What",
        "Where",
        "When",
        "Which",
        "Who",
        "Whose",
        "Why",
        "How",
        "Does",
        "Do",
        "Did",
        "Is",
        "Are",
        "Was",
        "Were",
        "Has",
        "Have",
        "Had",
        "The",
        "This",
        "That",
        "These",
        "Those",
        "Would",
        "Could",
        "Should",
        "Will",
        "Can",
        "May",
        "Might",
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ];
    let mut out: Vec<String> = Vec::new();
    for tok in question.split(|c: char| !c.is_alphanumeric()) {
        if tok.len() < 3 {
            continue;
        }
        let mut chars = tok.chars();
        if !chars.next().map(|c| c.is_uppercase()).unwrap_or(false) {
            continue;
        }
        if STOP.iter().any(|s| s.eq_ignore_ascii_case(tok)) {
            continue;
        }
        if !out.iter().any(|e| e == tok) {
            out.push(tok.to_string());
        }
        if out.len() >= 2 {
            break;
        }
    }
    out
}

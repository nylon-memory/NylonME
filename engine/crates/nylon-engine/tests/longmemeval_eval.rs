//! LongMemEval-S 检索/作答评测：把每个实例的 haystack sessions 逐 session 织入引擎，
//! 对该实例问题跑 Resonate，统计答案会话（answer_session_ids）轮次是否出现在
//! 激活结果前 10（recall@10）；NYLON_EVAL_E2E=1 时 LLM 作答 + 双裁判。
//!
//! 数据集：https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned
//!   （longmemeval_s_cleaned.json，500 实例，中位 ~48 session × ~10 turn）
//! 用法：
//!   $env:NYLON_LME_PATH="D:\data\longmemeval_s_cleaned.json"
//!   cargo test --release -p nylon-engine --test longmemeval_eval -- --ignored --nocapture
//!
//! 口径设计（2026-09-23，对齐 LoCoMo 权威配置）：
//! - owner = question_id，tenant = "longmemeval"，事件 id = "s{i}t{j}"（session i 第 j 轮）
//! - 证据 = answer_session_ids 命中会话的全部非空轮次（any-hit / all-hit 双口径，同 LoCoMo）
//! - 叶子文本前挂会话日期锚 [haystack_dates[i]]；作答/裁判的问题附 question_date
//! - 联想深度按题型自适应：single-session-* 仅种子（max_hops=0，对应 LoCoMo Cat4 结论），
//!   multi-session / temporal-reasoning / knowledge-update 走默认扩散
//! - 编织并发：实例内 session 级并发（NYLON_LME_WEAVE_CONCURRENCY，默认 8），实例间串行；
//!   100 实例编织预计 ~3.5h（串行 ~27h 不可行）
//! - 编织缓存按 question_id 增量落盘（NYLON_EVAL_STORE_DIR），可中断续跑、可扩到 500
//! - 子采样：NYLON_LME_LIMIT=N 时按步长 500/N 取样，保题型分布

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

/// e2e 作答/裁判专用模型（与 locomo_eval 同源，见该文件注释）。
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

/// 网络抖动重试（与 locomo_eval 同源）。
/// 2026-10-08：单次 RPC 加 300s 硬超时（r5 干净跑在实例 4388e9dd 的活编织上
/// 挂死 25 分钟零进展——无 TCP/零 CPU/WAL 停写，无任何报错通道；超时后走重试，
/// 重试日志改用 println!（eprintln! 在 cargo test + PowerShell 重定向下不进日志）。
async fn rpc_with_retry<F, Fut, T>(what: &str, mut f: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<tonic::Response<T>, tonic::Status>>,
{
    let mut delay = 5u64;
    for attempt in 1..=8u32 {
        let out = tokio::time::timeout(std::time::Duration::from_secs(300), f()).await;
        let res = match out {
            Ok(r) => r,
            Err(_) => Err(tonic::Status::unavailable(format!(
                "{what} RPC 300s 硬超时（疑似引擎侧挂起）"
            ))),
        };
        match res {
            Ok(resp) => return resp.into_inner(),
            Err(e) => {
                if attempt == 8 {
                    panic!("{what} 重试 8 次仍失败: {e:?}");
                }
                println!("[eval] {what} 失败（第 {attempt}/8 次），{delay}s 后重试: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                delay = (delay * 3).min(120);
            }
        }
    }
    unreachable!()
}

/// e2e LLM 调用重试（与 locomo_eval 同源）。
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
                    println!("[eval] e2e LLM 调用重试 4 次仍失败: {e}");
                    return None;
                }
                println!("[eval] e2e LLM 调用失败（第 {attempt}/4 次），{delay}s 后重试: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                delay = (delay * 2).min(60);
            }
        }
    }
    None
}

/// 作答器（与 locomo_eval 同源；NYLON_EVAL_QA_PROMPT_V2=1 用反弃答+具体化提示）。
async fn answer_with_context(
    llm: Option<&dyn nylon_llm::ChatModel>,
    ctx: &str,
    question: &str,
) -> Option<String> {
    let llm = llm?;
    let v2 = std::env::var("NYLON_EVAL_QA_PROMPT_V2").is_ok();
    let system = if v2 {
        "You are an intelligent memory assistant tasked with retrieving accurate information from conversation memories. \
        Instructions: \
        1. Carefully analyze all provided memories; each memory may be prefixed with a timestamp like [2023/05/20 (Sat) 02:21], pay special attention to these timestamps. \
        2. If the memories contain contradictory information, prioritize the most recent memory. \
        3. For relative time references (like \"last year\" or \"two months ago\"), calculate the specific date, month, or year based on the memory timestamps and the question date. \
        4. Formulate a precise, concise answer based solely on the evidence in the memories. Prefer concrete details (names, numbers, dates, specific objects) over generic summaries. For questions asking what/which items, enumerate every relevant item mentioned in the memories. \
        5. Answer \"Not mentioned\" ONLY if none of the memories contain any information relevant to the question. If there is partial or indirect evidence, give your best grounded answer instead of abstaining. \
        Output ONLY valid JSON: {\"answer\": \"...\"}."
    } else {
        "You are an intelligent memory assistant tasked with retrieving accurate information from conversation memories. \
        Instructions: \
        1. Carefully analyze all provided memories; each memory may be prefixed with a timestamp like [2023/05/20 (Sat) 02:21], pay special attention to these timestamps. \
        2. If the memories contain contradictory information, prioritize the most recent memory. \
        3. For relative time references (like \"last year\" or \"two months ago\"), calculate the specific date, month, or year based on the memory timestamps and the question date. \
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

/// 裁判·论文口径（Mem0 Appendix A 从宽，与 locomo_eval 同源）。
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

/// 裁判·内部严格口径（与 locomo_eval 同源）。
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

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "需要 LongMemEval 数据集（NYLON_LME_PATH），手动运行"]
async fn longmemeval_recall() {
    let path = std::env::var("NYLON_LME_PATH")
        .expect("请设置 NYLON_LME_PATH 指向 longmemeval_s_cleaned.json");
    let limit: usize = std::env::var("NYLON_LME_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    let concurrency: usize = std::env::var("NYLON_LME_WEAVE_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    let dump_miss = std::env::var("NYLON_EVAL_DUMP_MISS").is_ok();
    let data: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("读取数据集失败"))
            .expect("解析 JSON 失败");
    let n_total = data.len();
    // 步长子采样：LIMIT=N 时每隔 n/N 取 1 个实例，保题型分布
    let indices: Vec<usize> = if limit >= n_total {
        (0..n_total).collect()
    } else {
        let step = n_total / limit;
        (0..limit).map(|k| k * step).collect()
    };
    println!(
        "[eval] LongMemEval-S: 总实例 {n_total}，本轮取 {} 个（步长采样），编织并发 {concurrency}",
        indices.len()
    );

    // 编织缓存（同 locomo_eval：按 owner 粒度增量落盘，编织侧配置变更必须换目录）
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
    let mut cache_map: HashMap<String, HashMap<String, Vec<u64>>> = cache_file
        .as_ref()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if !cache_map.is_empty() {
        println!(
            "[eval] 命中编织缓存（{} 个实例），逐实例复用",
            cache_map.len()
        );
    }

    eprintln!("[eval] 开始加载存储 {:?}", store_path);
    let t_load = std::time::Instant::now();
    let store = PersistentGraph::open(&store_path).unwrap();
    eprintln!("[eval] 存储加载完成 ({:?})", t_load.elapsed());
    let dims: usize = std::env::var("NYLON_EMBED_DIMS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(service::DEFAULT_EMBED_DIMS);
    let embedder = nylon_embed::embedder_from_env(dims);
    let embedder_on = embedder.is_some();
    let llm = llm_from_env();
    let llm_on = llm.is_some();
    let e2e = std::env::var("NYLON_EVAL_E2E").is_ok() && llm_on;
    let qa_llm = if e2e { qa_llm_from_env() } else { None };
    println!(
        "[eval] 嵌入: {} | 编织 LLM: {}",
        if embedder_on {
            "开"
        } else {
            "关（纯词面）"
        },
        if llm_on { "开" } else { "关（启发式）" }
    );
    let session_weave = std::env::var("NYLON_SESSION_WEAVE").is_ok() && llm_on;
    if !session_weave {
        panic!("LongMemEval 评测要求 NYLON_SESSION_WEAVE=1 且配置 NYLON_LLM_URL（双层写入口径）");
    }
    let svc_llm = llm.clone();
    println!("[eval] 构建 EngineService（含 HNSW 回填）...");
    let svc = EngineService::new(store, dims, embedder, svc_llm);
    println!("[eval] EngineService 就绪，绑定端口...");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(MemoryEngineServer::new(svc))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    let client = MemoryEngineClient::connect(addr).await.unwrap();
    println!("[eval] gRPC 客户端已连接，进入评测循环");

    let mut total = 0usize;
    let mut hit = 0usize;
    let mut seed_total_hit = 0usize;
    let mut all_total_hit = 0usize;
    let mut total_nodes = 0usize;
    let mut per_type: HashMap<String, (usize, usize, usize, usize)> = HashMap::new();
    let mut qa_total = 0usize;
    let mut qa_correct = 0usize;
    let mut qa_correct_strict = 0usize;
    let mut qa_per_type: HashMap<String, (usize, usize, usize)> = HashMap::new();
    let t0 = std::time::Instant::now();

    for (ord, &idx) in indices.iter().enumerate() {
        let inst = &data[idx];
        let qid = inst["question_id"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        // NYLON_LME_SKIP_FIRST=N：跳过前 N 个已评测实例（断点续跑；2026-10-08
        // 用于从 r5 干净跑挂死点 ord=487 恢复）。放在 loop 内而非切 indices，
        // 保持日志序号 (ord+1)/500 与主跑一致，便于两份日志直接合并统计。
        let skip_first: usize = std::env::var("NYLON_LME_SKIP_FIRST")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if ord < skip_first {
            continue;
        }
        let qtype = inst["question_type"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        // 题型过滤（NYLON_LME_ONLY_TYPE="multi-session,temporal-reasoning"）：
        // 配额受限时只跑目标题型的 A/B（2026-09-30 Kimi 5h 窗口 ≈ 90-120 次调用）。
        if let Ok(only) = std::env::var("NYLON_LME_ONLY_TYPE") {
            if !only.split(',').any(|t| t.trim() == qtype) {
                continue;
            }
        }
        let sessions = inst["haystack_sessions"]
            .as_array()
            .expect("haystack_sessions 应为数组");
        let session_ids: Vec<&str> = inst["haystack_session_ids"]
            .as_array()
            .expect("haystack_session_ids 应为数组")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        let dates: Vec<&str> = inst["haystack_dates"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let date_of = |i: usize| dates.get(i).copied().unwrap_or("");

        // 证据事件 id：answer_session_ids 命中会话的全部非空轮
        let answer_sids: Vec<&str> = inst["answer_session_ids"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let mut evidence: Vec<String> = Vec::new();
        for sid in &answer_sids {
            if let Some(i) = session_ids.iter().position(|s| s == sid) {
                for (j, turn) in sessions[i]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .enumerate()
                {
                    let content = turn["content"].as_str().unwrap_or("");
                    if !content.trim().is_empty() {
                        evidence.push(format!("s{i}t{j}"));
                    }
                }
            }
        }

        // 编织（缓存命中则跳过）
        let mut ev2nodes: HashMap<String, Vec<u64>>;
        if let Some(cached) = cache_map.get(&qid) {
            ev2nodes = cached.clone();
            println!(
                "[eval] {qid} ({}/{}) 复用缓存编织：{} 个事件映射",
                ord + 1,
                indices.len(),
                ev2nodes.len()
            );
        } else {
            // 打平 (session, events) 工作项，实例内并发编织
            let mut work: Vec<(usize, Vec<SessionEvent>)> = Vec::new();
            for (i, sess) in sessions.iter().enumerate() {
                let date_anchor = date_of(i);
                let events: Vec<SessionEvent> = sess
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .enumerate()
                    .filter_map(|(j, turn)| {
                        let content = turn["content"].as_str().unwrap_or("");
                        if content.trim().is_empty() {
                            return None;
                        }
                        let role = turn["role"].as_str().unwrap_or("user");
                        Some(SessionEvent {
                            event_id: format!("s{i}t{j}"),
                            speaker: role.to_string(),
                            text: if date_anchor.is_empty() {
                                content.to_string()
                            } else {
                                format!("[{date_anchor}] {content}")
                            },
                        })
                    })
                    .collect();
                if !events.is_empty() {
                    work.push((i, events));
                }
            }
            let skip_abstract = std::env::var("NYLON_EVAL_SKIP_ABSTRACT").is_ok();
            // 实例内滑动窗口并发（JoinSet，不加新依赖）：窗口大小 = concurrency
            let job_total = work.len();
            let mut results: Vec<WeaveSessionResponse> = Vec::new();
            let mut set: tokio::task::JoinSet<WeaveSessionResponse> = tokio::task::JoinSet::new();
            let mut iter = work.into_iter();
            let mut spawn_one =
                |iter: &mut std::vec::IntoIter<(usize, Vec<SessionEvent>)>,
                 set: &mut tokio::task::JoinSet<WeaveSessionResponse>| {
                    if let Some((_i, events)) = iter.next() {
                        let skip_abstract = skip_abstract; // Copy 出闭包局部，满足 spawn 'static
                        let c = client.clone();
                        let owner = qid.clone();
                        set.spawn(async move {
                            rpc_with_retry("weave_session", || {
                                let mut c = c.clone();
                                let events = events.clone();
                                let owner = owner.clone();
                                async move {
                                    c.weave_session(WeaveSessionRequest {
                                        tenant_id: "longmemeval".into(),
                                        owner_id: owner,
                                        events,
                                        skip_abstract,
                                    })
                                    .await
                                }
                            })
                            .await
                        });
                    }
                };
            for _ in 0..concurrency {
                spawn_one(&mut iter, &mut set);
            }
            let mut jobs_done = 0usize;
            while let Some(res) = set.join_next().await {
                jobs_done += 1;
                println!(
                    "[eval] {qid} ({}/{}) weave_session 完成 {jobs_done}/{job_total}",
                    ord + 1,
                    indices.len()
                );
                results.push(res.expect("weave_session 任务 panic"));
                spawn_one(&mut iter, &mut set);
            }
            ev2nodes = HashMap::new();
            let mut inst_nodes = 0usize;
            for resp in &results {
                for en in &resp.leaf_nodes {
                    if !en.event_id.is_empty() {
                        ev2nodes
                            .entry(en.event_id.clone())
                            .or_default()
                            .push(en.node_id);
                    }
                }
                for f in &resp.fact_nodes {
                    for sid in &f.source_event_ids {
                        ev2nodes.entry(sid.clone()).or_default().push(f.node_id);
                    }
                }
                inst_nodes += resp.leaf_nodes.len() + resp.fact_nodes.len();
            }
            total_nodes += inst_nodes;
            if let Some(f) = &cache_file {
                cache_map.insert(qid.clone(), ev2nodes.clone());
                std::fs::write(f, serde_json::to_string(&cache_map).unwrap())
                    .expect("写编织缓存失败");
            }
            println!(
                "[eval] {qid} ({}/{}) 双层写入 {inst_nodes} 节点（累计 {total_nodes}，{} session，{:?}）",
                ord + 1,
                indices.len(),
                sessions.len(),
                t0.elapsed()
            );
            if std::env::var("NYLON_WORLD_BRIDGES_ASYNC").is_ok() {
                let wait_secs = std::env::var("NYLON_REFLECT_WAIT_SECS")
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(45);
                tokio::time::sleep(std::time::Duration::from_secs(wait_secs)).await;
            }
        }

        // 查询
        let question = inst["question"].as_str().unwrap_or("");
        let question_date = inst["question_date"].as_str().unwrap_or("");
        // 联想深度按题型自适应：single-session-* 仅种子（LoCoMo Cat4 结论平移）
        // 作答预算加大（NYLON_EVAL_CTX_K=15/20）：证据在池里（多会话错题 74 轮证据
        // 已在 Top-10、70 轮在 11-32 位），Top-10 装不下完整枚举。只扩大作答上下文，
        // 不动 recall@10 统计口径。
        let ctx_k: usize = std::env::var("NYLON_EVAL_CTX_K")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(RECALL_K);
        let hops: Option<u32> = if qtype.starts_with("single-session") {
            Some(0)
        } else {
            None
        };
        let resp = rpc_with_retry("resonate", || {
            let mut c = client.clone();
            let owner = qid.clone();
            let query = question.to_string();
            async move {
                c.resonate(ResonateRequest {
                    tenant_id: "longmemeval".into(),
                    owner_id: owner,
                    query,
                    context: hops.map(|h| ContextSpectrum {
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
        // 会话多样性重排（NYLON_EVAL_SESSION_DIV=1，题型名单 NYLON_EVAL_SESSION_DIV_TYPES
        // 默认仅 multi-session）。诊断（2026-09-30 round3）：multi-session J=63%，
        // any-hit 100% 但 all-hit 0/27——Top-10 被单一证据会话占满，计数/聚合题
        // 只能看到一半证据。第一轮按分数顺序每个会话取最优节点（覆盖广度），
        // 第二轮按分数补满 Top-10。单会话题不开（证据本就集中，重排会稀释）。
        let session_div = std::env::var("NYLON_EVAL_SESSION_DIV").is_ok() && {
            let types = std::env::var("NYLON_EVAL_SESSION_DIV_TYPES")
                .unwrap_or_else(|_| "multi-session".into());
            types.split(',').any(|t| t.trim() == qtype)
        };
        let resp = if session_div {
            // node → session 索引（事件 id 形如 "s{i}t{j}"；事实节点经 source_event_ids 映射）
            let mut node2sess: HashMap<u64, usize> = HashMap::new();
            for (ev, ns) in &ev2nodes {
                let si = ev
                    .strip_prefix('s')
                    .and_then(|r| r.split('t').next())
                    .and_then(|s| s.parse::<usize>().ok());
                if let Some(i) = si {
                    for n in ns {
                        node2sess.insert(*n, i);
                    }
                }
            }
            let mut covered: std::collections::HashSet<usize> = std::collections::HashSet::new();
            let mut out: Vec<ActivatedNode> = Vec::new();
            let mut rest: Vec<ActivatedNode> = Vec::new();
            for a in resp.activated.into_iter() {
                match node2sess.get(&a.node_id) {
                    Some(i) if !covered.contains(i) && out.len() < RECALL_K => {
                        covered.insert(*i);
                        out.push(a);
                    }
                    _ => rest.push(a),
                }
            }
            if out.len() < RECALL_K {
                out.extend(rest.into_iter().take(RECALL_K - out.len()));
            }
            ResonateResponse {
                activated: out,
                seed_ids: resp.seed_ids,
            }
        } else {
            resp
        };
        let got: Vec<u64> = resp
            .activated
            .iter()
            .take(RECALL_K)
            .map(|a| a.node_id)
            .collect();
        let ok = evidence.iter().any(|e| {
            ev2nodes
                .get(e)
                .map(|ns| ns.iter().any(|n| got.contains(n)))
                .unwrap_or(false)
        });
        let all_hit = !evidence.is_empty()
            && evidence.iter().all(|e| {
                ev2nodes
                    .get(e)
                    .map(|ns| ns.iter().any(|n| got.contains(n)))
                    .unwrap_or(false)
            });
        let seed_hit = evidence.iter().any(|e| {
            ev2nodes
                .get(e)
                .map(|ns| ns.iter().any(|n| resp.seed_ids.contains(n)))
                .unwrap_or(false)
        });
        total += 1;
        if ok {
            hit += 1;
        }
        if all_hit {
            all_total_hit += 1;
        }
        if seed_hit {
            seed_total_hit += 1;
        }
        let entry = per_type.entry(qtype.clone()).or_insert((0, 0, 0, 0));
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

        // e2e：top-10 检索内容 → LLM 作答（附 question_date）→ 双裁判
        let mut qa_line = String::new();
        if e2e {
            let gold = match &inst["answer"] {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if !gold.trim().is_empty() {
                let ctx_items: Vec<String> = resp
                    .activated
                    .iter()
                    .take(ctx_k)
                    .filter_map(|a| a.filaments.as_ref().map(|f| f.fact.clone()))
                    .collect();
                let ctx_text = ctx_items.join("\n");
                let dated_q = if question_date.is_empty() {
                    question.to_string()
                } else {
                    format!("{question} (question date: {question_date})")
                };
                let candidate = answer_with_context(qa_llm.as_deref(), &ctx_text, &dated_q).await;
                let (correct, correct_strict) = match &candidate {
                    Some(ans) => {
                        let p = judge_answer_paper(qa_llm.as_deref(), &dated_q, &gold, ans)
                            .await
                            .unwrap_or(false);
                        // NYLON_EVAL_SINGLE_JUDGE=1：跳过严格裁判（配额受限 A/B 只看 J 口径）
                        let s = if std::env::var("NYLON_EVAL_SINGLE_JUDGE").is_ok() {
                            p
                        } else {
                            judge_answer_strict(qa_llm.as_deref(), &dated_q, &gold, ans)
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
                let e = qa_per_type.entry(qtype.clone()).or_insert((0, 0, 0));
                e.0 += 1;
                if correct {
                    e.1 += 1;
                }
                if correct_strict {
                    e.2 += 1;
                }
                qa_line = format!(" | J={} 严格={}", correct as u8, correct_strict as u8);
                if dump_miss && !correct {
                    println!("\n[QA-WRONG] {qid} type={qtype} recall_hit={ok} all_hit={all_hit}");
                    println!("  Q: {dated_q}");
                    println!("  gold: {gold}");
                    println!("  ours: {}", candidate.as_deref().unwrap_or("<无答案>"));
                    if std::env::var("NYLON_EVAL_DUMP_CTX").is_ok() {
                        for ev in &evidence {
                            let pos = ev2nodes.get(ev).and_then(|ns| {
                                ns.iter()
                                    .filter_map(|n| {
                                        resp.activated
                                            .iter()
                                            .position(|a| a.node_id == *n)
                                            .map(|p| p + 1)
                                    })
                                    .min()
                            });
                            println!(
                                "  Epos[{ev}]: woven={} pos={pos:?}",
                                ev2nodes.contains_key(ev)
                            );
                        }
                        println!("  ctx:");
                        for (i, c) in ctx_items.iter().enumerate() {
                            let snip: String = c.chars().take(110).collect();
                            println!("   {:>2}. {snip}", i + 1);
                        }
                    }
                }
            }
        }
        println!(
            "[eval] {qid} ({}/{}) type={qtype} recall={} seed={} all={}{qa_line}",
            ord + 1,
            indices.len(),
            ok as u8,
            seed_hit as u8,
            all_hit as u8
        );
        if dump_miss && !ok {
            let evidence_pos = evidence
                .iter()
                .filter_map(|e| ev2nodes.get(e))
                .flatten()
                .filter_map(|n| {
                    resp.activated
                        .iter()
                        .position(|a| a.node_id == *n)
                        .map(|p| p + 1)
                })
                .min();
            println!(
                "\n[MISS] {qid} type={qtype} seed_hit={seed_hit} evidence_pos={evidence_pos:?}"
            );
            println!("  Q: {question}");
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

    println!();
    println!(
        "=== LongMemEval-S 评测（证据召回 recall@{RECALL_K}, {} 口径） ===",
        if embedder_on {
            "词面+向量融合"
        } else {
            "纯词面"
        }
    );
    println!(
        "实例数: {}, 织入节点: {total_nodes}, 耗时: {:?}",
        indices.len(),
        t0.elapsed()
    );
    if total > 0 {
        println!(
            "有效 QA: {total}, any-hit: {hit} = {:.1}% | all-hit: {all_total_hit} = {:.1}% | 种子: {seed_total_hit} = {:.1}%",
            hit as f64 / total as f64 * 100.0,
            all_total_hit as f64 / total as f64 * 100.0,
            seed_total_hit as f64 / total as f64 * 100.0
        );
    }
    let mut types: Vec<_> = per_type.iter().collect();
    types.sort_by(|a, b| a.0.cmp(b.0));
    for (ty, (t, h, sh, ah)) in &types {
        println!(
            "  {ty}: 最终 {h}/{t} = {:.1}% | 种子 {sh}/{t} = {:.1}% | 全证据 {ah}/{t} = {:.1}%",
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
        let mut qtypes: Vec<_> = qa_per_type.iter().collect();
        qtypes.sort_by(|a, b| a.0.cmp(b.0));
        for (ty, (t, c, cs)) in &qtypes {
            println!(
                "  {ty}: J {c}/{t} = {:.1}% | 严格 {cs}/{t} = {:.1}%",
                *c as f64 / *t as f64 * 100.0,
                *cs as f64 / *t as f64 * 100.0
            );
        }
    }
}

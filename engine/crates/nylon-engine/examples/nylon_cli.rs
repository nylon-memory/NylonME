//! NylonME 通用 CLI：weave / resonate / get，供 Codex 插件经 plink 远程调用。
#[path = "../src/audit.rs"]
mod audit;

#[path = "../src/auth.rs"]
mod auth;

#[path = "../src/service.rs"]
mod service;

use service::pb::memory_engine_client::MemoryEngineClient;
use service::pb::*;

fn arg(args: &[String], key: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == key).map(|w| w[1].clone())
}

/// 远端启用 API key 鉴权（L2.2）时随请求携带 x-api-key。
fn sign<T>(mut req: tonic::Request<T>, key: &Option<String>) -> tonic::Request<T> {
    if let Some(k) = key {
        if let Ok(v) = k.parse() {
            req.metadata_mut().insert("x-api-key", v);
        }
    }
    req
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("help");
    let addr = std::env::var("NYLON_SERVER").unwrap_or_else(|_| "http://127.0.0.1:50051".into());
    let tenant = arg(&args, "--tenant").unwrap_or_else(|| "codex".into());
    let owner = arg(&args, "--owner").unwrap_or_else(|| "default".into());
    let task = arg(&args, "--task");
    let mut client = MemoryEngineClient::connect(addr)
        .await
        .expect("connect failed");
    let api_key = std::env::var("NYLON_API_KEY").ok();
    let ctx = |hops: Option<u32>| {
        Some(ContextSpectrum {
            task: task.clone(),
            emotion_valence: None,
            device: None,
            max_hops: hops,
        })
    };

    match cmd {
        "weave" => {
            let fact = arg(&args, "--fact").expect("--fact required");
            let r = client.weave(sign(tonic::Request::new(WeaveRequest { tenant_id: tenant, owner_id: owner, raw_event: fact, context: ctx(None) }), &api_key)).await.expect("weave failed").into_inner();
            println!("NODE_ID={}", r.node_id);
            println!("LINKED={:?}", r.linked_nodes);
        }
        "resonate" => {
            let query = arg(&args, "--query").expect("--query required");
            let budget: u32 = arg(&args, "--budget").and_then(|s| s.parse().ok()).unwrap_or(8);
            let hops: Option<u32> = arg(&args, "--hops").and_then(|s| s.parse().ok());
            let r = client.resonate(sign(tonic::Request::new(ResonateRequest { tenant_id: tenant.clone(), owner_id: owner, query, context: ctx(hops), budget }), &api_key)).await.expect("resonate failed").into_inner();
            println!("ACTIVATED={}", r.activated.len());
            for n in &r.activated {
                let g = client.get_node(sign(tonic::Request::new(GetNodeRequest { tenant_id: tenant.clone(), node_id: n.node_id }), &api_key)).await;
                let fact = g.ok().and_then(|x| x.into_inner().filaments).map(|f| f.fact).unwrap_or_default();
                println!("{}\t{:.3}\t{}", n.node_id, n.resonance, fact);
            }
        }
        "get" => {
            let id: u64 = arg(&args, "--id").expect("--id required").parse().expect("--id must be u64");
            let g = client.get_node(sign(tonic::Request::new(GetNodeRequest { tenant_id: tenant, node_id: id }), &api_key)).await.expect("get failed").into_inner();
            if let Some(f) = g.filaments { println!("{}", f.fact); }
        }
        _ => println!("usage: nylon_cli weave|resonate|get [--tenant T] [--owner O] [--task T] [--fact F | --query Q [--budget N] [--hops N] | --id N]"),
    }
}

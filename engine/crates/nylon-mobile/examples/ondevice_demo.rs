//! 端上真机冒烟测试：通过 MobileEngine 公开 API 走完整链路。
//!
//! 用法: ondevice_demo <data_dir> <phase>
//!   phase 1 = 编织一批中英混合日常对话记忆 + checkpoint
//!   phase 2 = 重新打开库（验证 WAL/快照跨进程恢复），resonate 查询并打印耗时
//!
//! 设计为 adb push 后直接在 /data/local/tmp 下运行（shell 用户可写）。

use std::sync::Arc;
use std::time::Instant;

use nylon_mobile::{MobileConfig, MobileEngine};

const FACTS: &[&str] = &[
    "用户每天早上七点起床跑步三公里",
    "用户养了一只三岁的橘猫，叫咪咪",
    "用户去年十月从杭州搬到成都工作",
    "用户的女朋友是小学老师，教语文",
    "用户对花生过敏，吃一点就会起疹子",
    "用户周末喜欢去玉林路那边喝咖啡写代码",
    "用户的车是 2023 款比亚迪汉，白色",
    "用户计划明年五一去日本看樱花",
    "用户妈妈生日是农历八月十五",
    "用户正在自学 Rust，想做开源项目",
    "User prefers oat milk latte over regular milk",
    "User's dentist appointment is on the 25th of every month",
    "用户信用卡还款日是每月十号",
    "用户 apartment 的 wifi 密码是咖啡店的电话",
    "用户打羽毛球，球拍是李宁的雷霆 90",
    "用户不吃香菜，吃火锅必点毛肚",
    "用户的直属领导叫王芳，负责华东区",
    "用户周五晚上固定和大学室友开黑打 DOTA2",
    "用户近视四百度，散光一百度",
    "用户答应妹妹明年帮她参谋高考志愿",
];

const QUERIES: &[&str] = &[
    "用户的猫",
    "早上跑步",
    "搬到哪里工作",
    "过敏",
    "咖啡",
    "女朋友",
    "日本",
    "周五晚上",
];

fn open(dir: &str) -> Arc<MobileEngine> {
    MobileEngine::new(MobileConfig {
        data_dir: dir.into(),
        owner: "demo-user".into(),
    })
    .unwrap_or_else(|e| {
        eprintln!("open failed: {e}");
        std::process::exit(1);
    })
}

fn dir_stats(dir: &str) -> (usize, u64) {
    let mut files = 0usize;
    let mut bytes = 0u64;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if let Ok(m) = e.metadata() {
                if m.is_file() {
                    files += 1;
                    bytes += m.len();
                }
            }
        }
    }
    (files, bytes)
}

fn phase1(dir: &str) {
    let engine = open(dir);
    let t0 = Instant::now();
    for (i, f) in FACTS.iter().enumerate() {
        let t = Instant::now();
        let id = engine
            .weave(f.to_string(), None, None)
            .unwrap_or_else(|e| {
                eprintln!("weave[{i}] failed: {e}");
                std::process::exit(1);
            });
        println!("weave[{i:02}] id={id} took {:?}", t.elapsed());
    }
    let tc = Instant::now();
    engine.checkpoint().expect("checkpoint");
    println!("checkpoint took {:?}", tc.elapsed());
    let (files, bytes) = dir_stats(dir);
    println!(
        "phase1 done: {} facts woven in {:?}, data dir: {files} files / {bytes} bytes",
        FACTS.len(),
        t0.elapsed()
    );
}

fn phase2(dir: &str) {
    let engine = open(dir);
    println!("store reopened (WAL/snapshot restore ok)");
    let count = engine.node_count().unwrap();
    println!("node_count = {count} (expect {})", FACTS.len());
    let mut total = std::time::Duration::ZERO;
    for q in QUERIES {
        let t = Instant::now();
        let hits = engine.resonate(q.to_string(), 3).unwrap_or_else(|e| {
            eprintln!("resonate[{q}] failed: {e}");
            std::process::exit(1);
        });
        let el = t.elapsed();
        total += el;
        let top: Vec<String> = hits
            .iter()
            .map(|h| format!("{:.2}|{}", h.resonance, h.fact.chars().take(24).collect::<String>()))
            .collect();
        println!("resonate[{q}] {el:?} -> {top:?}");
    }
    println!("phase2 done: {} queries, avg {:?}", QUERIES.len(), total / QUERIES.len() as u32);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: ondevice_demo <data_dir> <1|2>");
        std::process::exit(2);
    }
    let dir = args[1].clone();
    match args[2].as_str() {
        "1" => phase1(&dir),
        "2" => phase2(&dir),
        other => {
            eprintln!("unknown phase: {other}");
            std::process::exit(2);
        }
    }
}

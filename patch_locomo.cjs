const fs = require('fs');
const path = 'D:/Project/NylonMem/nylon/engine/crates/nylon-engine/tests/locomo_eval.rs';
let s = fs.readFileSync(path, 'utf8');

// Add LLM import
s = s.replace(
  'use nylon_storage::PersistentGraph;',
  'use nylon_llm::llm_from_env;\nuse nylon_storage::PersistentGraph;'
);

// Add LLM detection after embedder detection
s = s.replace(
  'let embedder_on = embedder.is_some();',
  'let embedder_on = embedder.is_some();\n    let llm = llm_from_env();\n    let llm_on = llm.is_some();'
);

// Add LLM status print
s = s.replace(
  'println!("[eval] 未配置 NYLON_EMBED_URL，走纯词面口径 (dims={dims})");',
  'println!("[eval] 未配置 NYLON_EMBED_URL，走纯词面口径 (dims={dims})");\n    if llm_on {\n        println!("[eval] LLM 通道已启用 (NYLON_LLM_URL)，编织分解开启");\n    } else {\n        println!("[eval] 未配置 NYLON_LLM_URL，走启发式分解");\n    }'
);

// Also add for the embedder-on path
s = s.replace(
  'println!("[eval] 嵌入通道已启用 (NYLON_EMBED_URL), dims={dims}");',
  'println!("[eval] 嵌入通道已启用 (NYLON_EMBED_URL), dims={dims}");\n    if llm_on {\n        println!("[eval] LLM 通道已启用 (NYLON_LLM_URL)，编织分解开启");\n    } else {\n        println!("[eval] 未配置 NYLON_LLM_URL，走启发式分解");\n    }'
);

// Pass LLM to EngineService
s = s.replace(
  'let svc = EngineService::new(store, dims, embedder, None);',
  'let svc = EngineService::new(store, dims, embedder, llm);'
);

// Update the aggregate output to mention LLM
s = s.replace(
  'println!("=== LoCoMo 子集评估（证据召回 recall@{RECALL_K}, {} 口径）===", if embedder_on { "词面+向量融合" } else { "纯词面" });',
  'println!("=== LoCoMo 子集评估（证据召回 recall@{RECALL_K}, embed={}, llm={}）===", if embedder_on { "on" } else { "off" }, if llm_on { "on" } else { "off" });'
);

fs.writeFileSync(path, s, 'utf8');
console.log('locomo eval updated with LLM support');

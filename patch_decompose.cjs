const fs = require('fs');
const path = 'D:/Project/NylonMem/nylon/engine/crates/nylon-engine/src/service.rs';
let s = fs.readFileSync(path, 'utf8');

// === Step 1: Insert decompose function before judge_conflicts ===
const DECOMPOSE_FN = 
/// LLM \u7f16\u7ec7\u5206\u89e3\uff1a\u4ece\u539f\u59cb\u4e8b\u4ef6\u63d0\u53d6\u516d\u4e1d\u7ed3\u6784\u3002
/// \u8fd4\u56de None \u8868\u793a LLM \u4e0d\u53ef\u7528\u6216\u5931\u8d25\uff0c\u8c03\u7528\u65b9\u56de\u9000\u5230\u542f\u53d1\u5f0f\u5206\u89e3\u3002
async fn decompose(llm: \x26dyn ChatModel, raw_event: \x26str) -\x3e Option\x3c(String, Vec\x3cString\x3e, f32, f32, f32)\x3e {
    let system = \x22You are a memory extraction engine. Given a raw event or statement, extract structured memory filaments. Output ONLY valid JSON with: fact (concise factual statement preserving key details), relations (array of topic tags e.g. [\\x22travel\\x22, \\x22food\\x22]), emotion_valence (float -1.0 to 1.0, negative=unpleasant), emotion_intensity (float 0.0 to 1.0), confidence (float 0.0 to 1.0).\x22;
    let user = format!(\\x22Raw event: {raw_event}\\x22);
    match llm.chat_json(system, \x26user).await {
        Ok(v) =\x3e {
            let fact = v.get(\\x22fact\\x22).and_then(|f| f.as_str()).unwrap_or(raw_event).to_string();
            let relations: Vec\x3cString\x3e = v.get(\\x22relations\\x22)
                .and_then(|r| r.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            let valence = v.get(\\x22emotion_valence\\x22).and_then(|f| f.as_f64()).map(|f| f as f32).unwrap_or(0.0)
                .clamp(-1.0, 1.0);
            let intensity = v.get(\\x22emotion_intensity\\x22).and_then(|f| f.as_f64()).map(|f| f as f32).unwrap_or(0.5)
                .clamp(0.0, 1.0);
            let confidence = v.get(\\x22confidence\\x22).and_then(|f| f.as_f64()).map(|f| f as f32).unwrap_or(0.8)
                .clamp(0.0, 1.0);
            Some((fact, relations, valence, intensity, confidence))
        }
        Err(_) =\x3e None,
    }
}

;

// Insert before judge_conflicts
s = s.replace(
  '\n/// LLM conflict detection',
  DECOMPOSE_FN + '/// LLM conflict detection'
);

// === Step 2: Add decompose call in weave(), before building the node ===
// Current code creates relations from ctx.task, then node, then embed.
// We need to replace the filament construction section.

// The section to replace:
//   let relations: Vec<String> = ctx.task.clone().into_iter().collect();
//   let mut node = MemoryNode { ... naive filaments ... };

const OLD_SECTION = '        let relations: Vec<String> = ctx.task.clone().into_iter().collect();\n        let mut node = MemoryNode {';

const NEW_SECTION =         // LLM \u7f16\u7ec7\u5206\u89e3\uff08\u9501\u5916\uff0c\u66ff\u6362\u542f\u53d1\u5f0f\u5206\u89e3\uff09
        let (fact, relations, emotion_valence, emotion_intensity, confidence) = if let Some(llm) = \x26self.llm {
            decompose(llm.as_ref(), \x26r.raw_event).await.unwrap_or_else(|| {
                let rels: Vec\x3cString\x3e = ctx.task.clone().into_iter().collect();
                (r.raw_event.clone(), rels, ctx.emotion_valence.unwrap_or(0.0), 0.5, 0.8)
            })
        } else {
            let rels: Vec\x3cString\x3e = ctx.task.clone().into_iter().collect();
            (r.raw_event.clone(), rels, ctx.emotion_valence.unwrap_or(0.0), 0.5, 0.8)
        };
        let mut node = MemoryNode {;

s = s.replace(OLD_SECTION, NEW_SECTION);

// === Step 3: Replace naive filament values with decomposed ones ===
// fact: r.raw_event.clone() -> fact
s = s.replace(
  '                fact: r.raw_event.clone(),',
  '                fact,'
);

// emotion_valence: ctx.emotion_valence.unwrap_or(0.0) -> emotion_valence
s = s.replace(
  '                emotion_valence: ctx.emotion_valence.unwrap_or(0.0),',
  '                emotion_valence,'
);

// emotion_intensity: 0.5 -> emotion_intensity
s = s.replace(
  '                emotion_intensity: 0.5,',
  '                emotion_intensity,'
);

// relations: relations.clone() -> relations.clone()
s = s.replace(
  '                relations: relations.clone(),',
  '                relations: relations.clone(),'  // unchanged, still uses relations
);

// confidence: 0.8 -> confidence
s = s.replace(
  '                confidence: 0.8,',
  '                confidence,'
);

// === Step 4: Update embed call to use decomposed fact instead of raw_event ===
s = s.replace(
  "                match emb.embed(std::slice::from_ref(&r.raw_event)).await {",
  "                match emb.embed(std::slice::from_ref(&fact)).await {"
);

fs.writeFileSync(path, s, 'utf8');
console.log('decomposition wired');

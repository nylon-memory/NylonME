const fs = require('fs');
const path = 'D:/Project/NylonMem/nylon/engine/crates/nylon-engine/src/service.rs';
let s = fs.readFileSync(path, 'utf8');

// Swap: move embed before node construction
// Current: decompose -> node (fact moved) -> embed (borrow after move)
// Target:  decompose -> embed -> node (fact moved)

// Find and replace the section
const oldNodeBlock = '        let mut node = MemoryNode {\n            id: 0,\n            owner_id: r.owner_id.clone(),\n            filaments: Filaments {\n                fact,\n                emotion_valence,\n                emotion_intensity,\n                created_at: now,\n                decay_rate: 0.01,\n                relations: relations.clone(),\n                confidence,\n                mentions_7d: 1,\n            },\n            tension: Tension { baseline: 1.0, last_updated: now },\n            embedding: Vec::new(),\n        };\n        // \u5d4c\u5165\u901a\u9053';

// Build the embed block + node block together
const embedBlock = '        // Embed the decomposed fact before moving it into the node\n        let embedding = if let Some(emb) = \x26self.embedder {\n            match emb.embed(std::slice::from_ref(\x26fact)).await {\n                Ok(mut v) =' + String.fromCharCode(62) + ' v.pop(),\n                Err(e) =' + String.fromCharCode(62) + ' return Err(Status::internal(format!("embed failed: {e}"))),\n            }\n        } else {\n            None\n        };\n        let mut node = MemoryNode {\n            id: 0,\n            owner_id: r.owner_id.clone(),\n            filaments: Filaments {\n                fact,\n                emotion_valence,\n                emotion_intensity,\n                created_at: now,\n                decay_rate: 0.01,\n                relations: relations.clone(),\n                confidence,\n                mentions_7d: 1,\n            },\n            tension: Tension { baseline: 1.0, last_updated: now },\n            embedding: Vec::new(),\n        };\n        //';

s = s.replace(oldNodeBlock, embedBlock);

// Now remove the duplicate embed block that comes after the node
const oldEmbedBlock = '        // \u5d4c\u5165\u901a\u9053\uff08\u9501\u5916\u8ba1\u7b97\uff0c\u907f\u514d\u6301\u9501\u7b49\u7f51\u7edc\uff09\n        let embedding = if let Some(emb) = \x26self.embedder {\n            match emb.embed(std::slice::from_ref(\x26fact)).await {\n                Ok(mut v) =' + String.fromCharCode(62) + ' v.pop(),\n                Err(e) =' + String.fromCharCode(62) + ' return Err(Status::internal(format!("embed failed: {e}"))),\n            }\n        } else {\n            None\n        };\n        let llm = self.llm.clone();';

// Wait, the comment might have Chinese characters. Let me find it differently.
// The duplicated embed block is between the node construction and the lock block.
// After our replacement, the structure is: ... node; /* ... */\n embed; ... llm; ... lock {
// But we already inserted embed before node. So we just need to remove the second embed.

// Find: after the node's closing "};" and before "let llm =", remove the embed block
// Simpler: replace the entire second embed + llm block with just llm

s = s.replace(
  '        // \u5d4c\u5165\u901a\u9053',
  '        // REMOVED_DUPLICATE'
);

// Now find what was placed and clean up
// The structure after our replacement should be:
// ... decompose ... embed ... node ... REMOVED_DUPLICATE ... second_embed ... llm ...

// Actually, let me take a completely different approach. Let me rebuild the entire
// section between decompose and the lock.

// Let me find the decompose result and the lock block start
const decompEnd = '        let mut node = MemoryNode {';
const lockStart = '        let llm = self.llm.clone();\n        let (local, linked, final_ticket, candidate_facts) = {';

// Replace everything between decompEnd (after our inserted embed+node) and lockStart (removing the duplicate embed)
// Actually this is getting too complex. Let me just replace the duplicated embed section.
// After our first replacement, the code should look like:
//   ... decompose -> embed -> node -> /*dup*/ embed -> llm -> lock
// We need: decompose -> embed -> node -> llm -> lock

// Let me use a simpler pattern: find the duplicate "let embedding =" and remove it
let lines = s.split('\n');
let newLines = [];
let foundFirstEmbed = false;
let skipNextEmbed = false;

for (let i = 0; i < lines.length; i++) {
  const line = lines[i];
  // If we see "let embedding = if let Some" and we already saw one, skip until "let llm"
  if (line.includes('let embedding = if let Some(emb)') && foundFirstEmbed) {
    skipNextEmbed = true;
    continue;
  }
  if (skipNextEmbed) {
    if (line.includes('let llm = self.llm.clone();')) {
      skipNextEmbed = false;
      newLines.push(line);
    }
    continue;
  }
  if (line.includes('let embedding = if let Some(emb)') && !foundFirstEmbed) {
    foundFirstEmbed = true;
  }
  newLines.push(line);
}

fs.writeFileSync(path, newLines.join('\n'), 'utf8');
console.log('swap done, lines:', newLines.length);

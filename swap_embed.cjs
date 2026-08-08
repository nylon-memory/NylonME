const fs = require('fs');
const path = 'D:/Project/NylonMem/nylon/engine/crates/nylon-engine/src/service.rs';
let lines = fs.readFileSync(path, 'utf8').split(/\r?\n/);

// Find all key line indices (0-indexed)
let nodeStart = -1, nodeEnd = -1, embedStart = -1, embedEnd = -1;

for (let i = 0; i < lines.length; i++) {
  if (lines[i].includes('let mut node = MemoryNode {') && nodeStart === -1) {
    nodeStart = i;
  }
  if (nodeStart !== -1 && nodeEnd === -1 && i > nodeStart && lines[i].trim() === '};') {
    nodeEnd = i;
  }
  if (lines[i].includes('let embedding = if let Some(emb) = ') && embedStart === -1) {
    embedStart = i;
  }
  if (embedStart !== -1 && embedEnd === -1 && i > embedStart && lines[i].trim() === '};') {
    embedEnd = i;
  }
}

console.log('nodeStart:', nodeStart, 'nodeEnd:', nodeEnd);
console.log('embedStart:', embedStart, 'embedEnd:', embedEnd);

if (nodeStart !== -1 && nodeEnd !== -1 && embedStart !== -1 && embedEnd !== -1) {
  // Extract the embed block (lines embedStart to embedEnd)
  let embedBlock = lines.slice(embedStart, embedEnd + 1);
  
  // Remove the embed block from its current position
  lines.splice(embedStart, embedEnd - embedStart + 1);
  
  // Calculate new nodeStart after removal (it shifts left)
  let newNodeStart = nodeStart < embedStart ? nodeStart : nodeStart - (embedEnd - embedStart + 1);
  
  // Insert embed block before node block
  // Also insert a comment line
  embedBlock.unshift('        // Embed the decomposed fact before moving it into the node');
  lines.splice(newNodeStart, 0, ...embedBlock);
  
  fs.writeFileSync(path, lines.join('\r\n'), 'utf8');
  console.log('swapped: embed now at line', newNodeStart + 1, 'node at line', newNodeStart + embedBlock.length + 1);
} else {
  console.log('could not find all markers');
}
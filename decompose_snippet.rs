/// LLM weave decomposition: extract six-filament structure from raw event.
/// Returns None when LLM unavailable/fails, caller falls back to heuristic decomposition.
async fn decompose(llm: &dyn ChatModel, raw_event: &str) -> Option<(String, Vec<String>, f32, f32, f32)> {
    let system = "You are a memory extraction engine. Given a raw event or statement, extract structured memory filaments. Output ONLY valid JSON with: fact (concise factual statement preserving key details), relations (array of topic tags e.g. [\"travel\", \"food\"]), emotion_valence (float -1.0 to 1.0, negative=unpleasant), emotion_intensity (float 0.0 to 1.0), confidence (float 0.0 to 1.0).";
    let user = format!("Raw event: {raw_event}");
    match llm.chat_json(system, &user).await {
        Ok(v) => {
            let fact = v.get("fact").and_then(|f| f.as_str()).unwrap_or(raw_event).to_string();
            let relations: Vec<String> = v.get("relations")
                .and_then(|r| r.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            let valence = v.get("emotion_valence").and_then(|f| f.as_f64()).map(|f| f as f32).unwrap_or(0.0)
                .clamp(-1.0, 1.0);
            let intensity = v.get("emotion_intensity").and_then(|f| f.as_f64()).map(|f| f as f32).unwrap_or(0.5)
                .clamp(0.0, 1.0);
            let confidence = v.get("confidence").and_then(|f| f.as_f64()).map(|f| f as f32).unwrap_or(0.8)
                .clamp(0.0, 1.0);
            Some((fact, relations, valence, intensity, confidence))
        }
        Err(_) => None,
    }
}

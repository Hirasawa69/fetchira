use serde_json::{json, Value};

use super::{uuid4, with_sources, Capability, Input, Outcome};
use crate::error::{Error, Result};

pub async fn call(
    base: &str,
    client: &wreq::Client,
    cap: Capability,
    input: &Input,
) -> Result<Outcome> {
    if !matches!(cap, Capability::Search | Capability::DeepResearch) {
        return Err(Error::Unsupported("perplexity_web"));
    }
    let query = input.need_query()?;
    let (mode, model) = select(cap, input);

    // Warm up the NextAuth session / let Cloudflare refresh __cf_bm.
    let _ = client.get(format!("{base}/api/auth/session")).send().await;

    let body = json!({
        "query_str": query,
        "params": {
            "attachments": [],
            "frontend_context_uuid": uuid4(),
            "frontend_uuid": uuid4(),
            "is_incognito": false,
            "language": "en-US",
            "last_backend_uuid": input.session.as_deref().map(Value::from).unwrap_or(Value::Null),
            "mode": mode,
            "model_preference": model,
            "source": "default",
            "sources": ["web"],
            "version": "2.18",
        }
    });

    let resp = client
        .post(format!("{base}/rest/sse/perplexity_ask"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    match status {
        401 | 403 => {
            return Err(Error::Provider {
                provider: "perplexity_web",
                status,
                body: "cloudflare/session; run `fetchira login perplexity_web`".into(),
            })
        }
        429 => return Err(Error::RateLimit("perplexity_web: rate limited".into())),
        _ => {}
    }
    parse(&text)
}

/// (mode, model_preference) from the capability plus optional `mode`/`model` overrides.
/// Free accounts ignore non-default models server-side; this is best-effort.
fn select(cap: Capability, input: &Input) -> (String, String) {
    let (mut mode, mut model) = match cap {
        Capability::DeepResearch => ("copilot", "pplx_alpha"),
        _ => ("concise", "turbo"),
    };
    if let Some(m) = input.mode.as_deref() {
        match m.to_ascii_lowercase().as_str() {
            "deep research" | "deep_research" | "research" => {
                (mode, model) = ("copilot", "pplx_alpha")
            }
            "pro" => (mode, model) = ("copilot", "pplx_pro"),
            "reasoning" => (mode, model) = ("copilot", "pplx_reasoning"),
            "search" | "auto" | "concise" => (mode, model) = ("concise", "turbo"),
            _ => {}
        }
    }
    let model = match input.model.as_deref() {
        Some(m) => {
            if mode == "concise" {
                mode = "copilot"; // a chosen model only applies in copilot mode
            }
            map_model(m)
        }
        None => model.to_string(),
    };
    (mode.to_string(), model)
}

/// Friendly model name -> Perplexity's internal `model_preference` id (these drift; raw passthrough otherwise).
fn map_model(m: &str) -> String {
    let k: String = m
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    match k.as_str() {
        "gpt5" | "gpt52" | "gpt55" => "gpt52",
        "claude" | "claudesonnet" | "claude45sonnet" => "claude45sonnet",
        "claudeopus" | "claudeopus48" => "claude45opus",
        "gemini" | "gemini3" | "gemini30pro" | "gemini31pro" => "gemini30pro",
        "sonar" | "sonar2" => "experimental",
        "kimi" | "kimik2" => "kimik2thinking",
        "grok" | "grok4" => "grok41reasoning",
        _ => return m.to_string(),
    }
    .to_string()
}

/// Parse the SSE stream: keep the last `event: message` payload, then dig out the final
/// answer + source URLs. Handles both the legacy `text[]/FINAL` and new `blocks[]` schemas.
fn parse(sse: &str) -> Result<Outcome> {
    let mut last: Option<Value> = None;
    for chunk in sse.split("\r\n\r\n") {
        if let Some(data) = chunk.trim_start().strip_prefix("event: message\r\ndata: ") {
            if let Ok(v) = serde_json::from_str::<Value>(data) {
                last = Some(v);
            }
        }
    }
    let msg = last.ok_or(Error::BadResponse("perplexity_web"))?;

    let mut answer = String::new();
    let mut sources: Vec<String> = Vec::new();

    // Legacy schema: msg.text is a JSON string holding a list of {step_type, content} steps.
    let steps: Value = match msg.get("text").and_then(|t| t.as_str()) {
        Some(s) => serde_json::from_str(s).unwrap_or(Value::Null),
        None => Value::Null,
    };
    if let Some(arr) = steps.as_array() {
        for st in arr {
            match st.get("step_type").and_then(|x| x.as_str()) {
                Some("SEARCH_RESULTS") => collect_sources(st.get("content"), &mut sources),
                Some("FINAL") => {
                    if let Some(a) = st
                        .get("content")
                        .and_then(|c| c.get("answer"))
                        .and_then(|x| x.as_str())
                    {
                        if let Ok(fin) = serde_json::from_str::<Value>(a) {
                            answer = fin
                                .get("answer")
                                .and_then(|x| x.as_str())
                                .unwrap_or_default()
                                .to_string();
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // New schema: top-level blocks[] with a markdown_block.
    if answer.is_empty() {
        if let Some(blocks) = msg.get("blocks").and_then(|b| b.as_array()) {
            for blk in blocks {
                if let Some(mb) = blk.get("markdown_block") {
                    answer = match mb.get("progress").and_then(|x| x.as_str()) {
                        Some("DONE") => mb
                            .get("answer")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        _ => mb
                            .get("chunks")
                            .and_then(|c| c.as_array())
                            .map(|cs| cs.iter().filter_map(|c| c.as_str()).collect())
                            .unwrap_or_default(),
                    };
                }
            }
        }
    }

    if answer.trim().is_empty() {
        return Err(Error::BadResponse("perplexity_web"));
    }
    let mut out = Outcome::new(with_sources(answer, &sources), 1);
    out.session = msg
        .get("backend_uuid")
        .and_then(|x| x.as_str())
        .map(str::to_string);
    Ok(out)
}

fn collect_sources(content: Option<&Value>, out: &mut Vec<String>) {
    if let Some(results) = content
        .and_then(|c| c.get("web_results"))
        .and_then(|w| w.as_array())
    {
        for r in results {
            if let Some(u) = r.get("url").and_then(|x| x.as_str()) {
                if !out.iter().any(|s| s == u) {
                    out.push(u.to_string());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_legacy_final_step() {
        let inner_final =
            json!({ "answer": "Rust async traits are stable.", "chunks": ["Rust ", "async"] })
                .to_string();
        let steps = json!([
            { "step_type": "SEARCH_RESULTS", "content": { "web_results": [{ "url": "https://doc.rust-lang.org", "title": "docs" }] } },
            { "step_type": "FINAL", "content": { "answer": inner_final } }
        ])
        .to_string();
        let msg = json!({ "backend_uuid": "x", "text": steps }).to_string();
        let sse =
            format!("event: message\r\ndata: {msg}\r\n\r\nevent: end_of_stream\r\ndata: {{}}");
        let out = parse(&sse).unwrap();
        assert!(out.text.contains("Rust async traits are stable."));
        assert!(out.text.contains("doc.rust-lang.org"));
    }

    #[test]
    fn parses_blocks_schema() {
        let msg = json!({ "blocks": [{ "markdown_block": { "progress": "DONE", "answer": "Final answer here", "chunks": [] } }] })
            .to_string();
        let sse = format!("event: message\r\ndata: {msg}\r\n\r\n");
        let out = parse(&sse).unwrap();
        assert!(out.text.contains("Final answer here"));
    }
}

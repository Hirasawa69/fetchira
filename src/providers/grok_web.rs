use base64::Engine;
use serde_json::{json, Value};

use super::{uuid4, with_sources, Capability, Input, LiveQuota, Outcome};
use crate::error::{Error, Result};

/// grok's anti-bot `x-statsig-id`. We send grok's own degraded-mode value: base64 of a thrown
/// `TypeError`, which xAI accepts when a real client's Statsig SDK fails to init. A *static* value
/// gets fingerprinted and 403'd, so randomize the error text each call. The real signed token (and
/// why we don't compute it) is documented in `research/grok-statsig/`.
fn statsig_id() -> String {
    let props = [
        "childNodes",
        "children",
        "firstChild",
        "parentNode",
        "nextSibling",
        "classList",
    ];
    let kinds = ["null", "undefined"];
    let msg = format!(
        "x1:TypeError: Cannot read properties of {} (reading '{}')",
        kinds[pick(kinds.len())],
        props[pick(props.len())],
    );
    base64::engine::general_purpose::STANDARD.encode(msg)
}

fn pick(n: usize) -> usize {
    use std::sync::atomic::{AtomicU64, Ordering};
    static C: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    ((t ^ C.fetch_add(0x9E37_79B9, Ordering::Relaxed)) as usize) % n
}

pub async fn call(
    base: &str,
    client: &wreq::Client,
    cap: Capability,
    input: &Input,
) -> Result<Outcome> {
    if !matches!(cap, Capability::Search | Capability::DeepResearch) {
        return Err(Error::Unsupported("grok_web"));
    }
    let query = input.need_query()?;
    let (model_name, preset, reasoning) = select(base, client, cap, input).await;

    // Resume an existing conversation, or start a new one.
    let url = match input.session.as_deref() {
        Some(conv) => format!("{base}/rest/app-chat/conversations/{conv}/responses"),
        None => format!("{base}/rest/app-chat/conversations/new"),
    };

    let body = json!({
        "temporary": false,
        "modelName": model_name,
        "message": query,
        "fileAttachments": [],
        "imageAttachments": [],
        "disableSearch": false,
        "enableImageGeneration": false,
        "returnImageBytes": false,
        "returnRawGrokInXaiRequest": false,
        "enableImageStreaming": false,
        "imageGenerationCount": 0,
        "forceConcise": false,
        "toolOverrides": {},
        "enableSideBySide": true,
        "sendFinalMetadata": true,
        "customInstructions": "",
        "deepsearchPreset": preset,
        "isReasoning": reasoning,
        "disableTextFollowUps": true,
    });

    let resp = client
        .post(url)
        // Body is JSON but grok.com sends it as text/plain — match the browser.
        .header("content-type", "text/plain;charset=UTF-8")
        .header(
            "baggage",
            "sentry-public_key=b311e0f2690c81f25e2c4cf6d4f7ce1c",
        )
        .header("x-statsig-id", statsig_id())
        .header("x-xai-request-id", uuid4())
        .body(body.to_string())
        .send()
        .await?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    match status {
        401 => {
            return Err(Error::Provider {
                provider: "grok_web",
                status,
                body: "session expired; run `fetchira login grok_web`".into(),
            })
        }
        403 => {
            // Anti-bot rejection (usually IP reputation / rate, not the cookie session). Re-login
            // won't help; the router fails over to other providers.
            return Err(Error::Provider {
                provider: "grok_web",
                status,
                body: "grok anti-bot rejected this request (IP/rate); failing over".into(),
            });
        }
        429 => return Err(Error::RateLimit("grok_web: rate limited".into())),
        _ => {}
    }
    parse(&text)
}

/// Live remaining budget grok.com's own web UI polls, for a given model. grok keys the quota by
/// MODEL (grok-4 ~40/2h, grok-4-heavy ~20/2h, grok-3 ~140/2h), not by request kind, so DEFAULT is
/// enough. The window is rolling (`windowSizeSeconds`).
pub async fn rate_limit(base: &str, client: &wreq::Client, model: &str) -> Result<LiveQuota> {
    let body = json!({ "requestKind": "DEFAULT", "modelName": model });
    let resp = client
        .post(format!("{base}/rest/rate-limits"))
        .header("content-type", "application/json")
        .header(
            "baggage",
            "sentry-public_key=b311e0f2690c81f25e2c4cf6d4f7ce1c",
        )
        .header("x-statsig-id", statsig_id())
        .header("x-xai-request-id", uuid4())
        .body(body.to_string())
        .send()
        .await?;
    if resp.status().as_u16() != 200 {
        return Err(Error::BadResponse("grok_web"));
    }
    let v: Value = serde_json::from_str(&resp.text().await.unwrap_or_default())
        .map_err(|_| Error::BadResponse("grok_web"))?;
    let n = |k: &str| v.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
    Ok(LiveQuota {
        remaining: n("remainingQueries"),
        total: n("totalQueries"),
        window_secs: n("windowSizeSeconds"),
    })
}

/// Pick (modelName, deepsearchPreset, isReasoning) for a grok call.
///
/// Default capability routing mirrors the web UI's reasoning modes:
///   search        -> Fast  (grok-4, no reasoning) — quick, fewer sources.
///   deep_research -> Heavy (grok-4-heavy) when the account has heavy budget, else Expert
///                    (grok-4 + reasoning). Heavy access/exhaustion is read live from grok's
///                    rate-limit endpoint, so an exhausted or unsubscribed Heavy degrades to Expert.
/// An explicit `mode` (auto/fast/expert/heavy/deepsearch) or `model` overrides the default.
async fn select(
    base: &str,
    client: &wreq::Client,
    cap: Capability,
    input: &Input,
) -> (String, &'static str, bool) {
    let (mut model, mut preset, mut reasoning): (String, &'static str, bool) = match cap {
        Capability::DeepResearch => {
            let heavy = rate_limit(base, client, "grok-4-heavy")
                .await
                .map(|lq| lq.remaining > 0)
                .unwrap_or(false);
            (
                (if heavy { "grok-4-heavy" } else { "grok-4" }).to_string(),
                "",
                true,
            )
        }
        _ => ("grok-4".to_string(), "", false),
    };
    if let Some(m) = input.mode.as_deref() {
        match m.to_ascii_lowercase().as_str() {
            "auto" | "fast" => {
                model = "grok-4".into();
                reasoning = false;
            }
            "expert" => {
                model = "grok-4".into();
                reasoning = true;
            }
            "heavy" => {
                model = "grok-4-heavy".into();
                reasoning = true;
            }
            "deepsearch" | "deep search" | "deep research" | "deep_research" | "research" => {
                preset = "default"
            }
            "deepersearch" | "deeper" => preset = "deeper",
            _ => {}
        }
    }
    if let Some(m) = input.model.as_deref() {
        let k: String = m
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        model = match k.as_str() {
            "grok3" => "grok-3".into(),
            "grok4" => "grok-4".into(),
            "grok4heavy" => "grok-4-heavy".into(),
            _ => model,
        };
    }
    (model, preset, reasoning)
}

/// Parse newline-delimited JSON. Prefer the terminal `modelResponse.message`; otherwise
/// concatenate streamed string `token`s. Collect `webSearchResults` as sources and the
/// `conversationId` as the resume token.
fn parse(ndjson: &str) -> Result<Outcome> {
    let mut tokens = String::new();
    let mut final_msg: Option<String> = None;
    let mut sources: Vec<String> = Vec::new();
    let mut conv: Option<String> = None;

    for line in ndjson.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("error").is_some() {
            return Err(Error::RateLimit("grok_web: stream error".into()));
        }
        if conv.is_none() {
            conv = find_str(&v, "conversationId");
        }
        let resp = match v.get("result").and_then(|r| r.get("response")) {
            Some(r) => r,
            None => continue,
        };
        if let Some(tok) = resp.get("token").and_then(|t| t.as_str()) {
            tokens.push_str(tok);
        }
        if let Some(m) = resp
            .get("modelResponse")
            .and_then(|m| m.get("message"))
            .and_then(|x| x.as_str())
        {
            final_msg = Some(m.to_string());
        }
        if let Some(results) = resp
            .get("webSearchResults")
            .and_then(|w| w.get("results"))
            .and_then(|r| r.as_array())
        {
            for r in results {
                if let Some(u) = r.get("url").and_then(|x| x.as_str()) {
                    if !sources.iter().any(|s| s == u) {
                        sources.push(u.to_string());
                    }
                }
            }
        }
    }

    let answer = final_msg.unwrap_or(tokens);
    if answer.trim().is_empty() {
        return Err(Error::BadResponse("grok_web"));
    }
    let mut out = Outcome::new(with_sources(strip_render(&answer), &sources), 1);
    out.session = conv;
    Ok(out)
}

/// Drop grok's inline `<grok:render …>…</grok:render>` citation-card markup.
fn strip_render(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("<grok:render") {
        out.push_str(&rest[..i]);
        match rest[i..].find("</grok:render>") {
            Some(j) => rest = &rest[i + j + "</grok:render>".len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Recursively find the first string value under `key` anywhere in the JSON.
fn find_str(v: &Value, key: &str) -> Option<String> {
    match v {
        Value::Object(o) => o
            .get(key)
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .or_else(|| o.values().find_map(|x| find_str(x, key))),
        Value::Array(a) => a.iter().find_map(|x| find_str(x, key)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ndjson_stream() {
        let lines = [
            r#"{"result":{"response":{"token":"Hello ","conversationId":"conv-99"}}}"#,
            r#"{"result":{"response":{"token":"world","webSearchResults":{"results":[{"title":"t","url":"https://x.ai","preview":"p"}]}}}}"#,
            r#"{"result":{"response":{"modelResponse":{"message":"Hello world, final."}}}}"#,
        ]
        .join("\n");
        let out = parse(&lines).unwrap();
        assert!(out.text.starts_with("Hello world, final."));
        assert!(out.text.contains("x.ai"));
        assert_eq!(out.session.as_deref(), Some("conv-99"));
    }

    #[test]
    fn stream_error_is_rate_limit() {
        let line = r#"{"error":{"code":429,"message":"rate"}}"#;
        assert!(matches!(parse(line), Err(Error::RateLimit(_))));
    }

    #[test]
    fn strips_render_cards() {
        let s = "Rust 1.96<grok:render card_id=\"x\"><argument>0</argument></grok:render> is out.";
        assert_eq!(strip_render(s), "Rust 1.96 is out.");
    }

    #[test]
    fn statsig_is_base64_x1() {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(statsig_id())
            .unwrap();
        assert!(String::from_utf8(raw).unwrap().starts_with("x1:TypeError"));
    }
}

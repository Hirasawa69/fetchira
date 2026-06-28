use serde_json::{json, Value};

use super::{uuid4, Capability, Input, Outcome};
use crate::error::{Error, Result};

pub async fn call(
    base: &str,
    client: &wreq::Client,
    cap: Capability,
    input: &Input,
) -> Result<Outcome> {
    if !matches!(cap, Capability::Search | Capability::DeepResearch) {
        return Err(Error::Unsupported("gemini_web"));
    }
    let q = input.need_query()?;

    // Deep research is a 3-step flow over this one endpoint, threaded via the session token:
    //   1. deep_research(query)        -> DR-flagged turn returns a PLAN + a `dr|<ids>` session
    //   2. deep_research(session, ...) -> "start" confirms+runs the research; other text edits the plan
    let raw_session = input.session.as_deref();
    let is_dr_run = raw_session.is_some_and(|s| s.starts_with("dr|"));
    let resume_meta = raw_session.map(|s| s.strip_prefix("dr|").unwrap_or(s));
    let want_dr_plan = matches!(cap, Capability::DeepResearch) && raw_session.is_none();
    let query = if is_dr_run
        && matches!(
            q.trim().to_ascii_lowercase().as_str(),
            "start" | "go" | "run" | ""
        ) {
        "Start research"
    } else {
        q
    };

    // Mint a fresh __Secure-1PSIDTS from __Secure-1PSID. The browser often hasn't been issued
    // this rotating companion at capture time, and without it /app renders logged-out (no
    // SNlM0e). Best-effort: the jar captures the Set-Cookie (Domain=.google.com) for the GET below.
    let _ = client
        .post("https://accounts.google.com/RotateCookies")
        .header("content-type", "application/json")
        .header("origin", "https://accounts.google.com")
        .body("[000,\"-0000000000000000000\"]")
        .send()
        .await;

    let page = client
        .get(format!("{base}/app"))
        .send()
        .await?
        .text()
        .await
        .unwrap_or_default();
    let at = scrape(&page, "SNlM0e").ok_or(Error::Provider {
        provider: "gemini_web",
        status: 0,
        body: "no session token; run `fetchira login gemini_web`".into(),
    })?;
    let bl = scrape(&page, "cfb2h").unwrap_or_default();
    let fsid = scrape(&page, "FdrFJe").unwrap_or_default();
    let hl = scrape(&page, "TuX5cc").unwrap_or_else(|| "en".into());

    let uuid = uuid4().to_uppercase();
    let mut inner_val = build_inner(query, &hl, &uuid, resume_meta);
    if want_dr_plan {
        if let Value::Array(a) = &mut inner_val {
            let blob: String = std::iter::repeat_with(|| uuid4().replace('-', ""))
                .take(82)
                .collect();
            a[3] = json!(format!("!{}", &blob[..2600.min(blob.len())]));
            a[4] = json!(uuid4().replace('-', ""));
            a[49] = json!(1);
            a[54] = json!([[[[[1]]]]]);
            a[55] = json!([[1]]);
        }
    }
    let inner = serde_json::to_string(&inner_val)?;
    let freq = serde_json::to_string(&json!([Value::Null, inner]))?;
    let url = format!(
        "{base}/_/BardChatUi/data/assistant.lamda.BardFrontendService/StreamGenerate\
         ?bl={bl}&f.sid={fsid}&hl={hl}&_reqid={}&rt=c",
        reqid()
    );

    let mut req = client
        .post(url)
        .header(
            "content-type",
            "application/x-www-form-urlencoded;charset=utf-8",
        )
        .header("origin", "https://gemini.google.com")
        .header("referer", "https://gemini.google.com/")
        .header("x-same-domain", "1")
        .header("x-goog-ext-525005358-jspb", format!("[\"{uuid}\",1]"));
    if let Some(id) = model_id(input.model.as_deref()) {
        req = req.header(
            "x-goog-ext-525001261-jspb",
            format!("[1,null,null,null,\"{id}\",null,null,0,[4]]"),
        );
    }
    let resp = req
        .body(form_encode(&[("at", &at), ("f.req", &freq)]))
        .send()
        .await?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    match status {
        400 | 401 => {
            return Err(Error::Provider {
                provider: "gemini_web",
                status,
                body: "session may be expired; run `fetchira login gemini_web`".into(),
            })
        }
        429 => return Err(Error::RateLimit("gemini_web: rate limited".into())),
        _ => {}
    }
    if want_dr_plan {
        parse_plan(&text)
    } else {
        parse(&text)
    }
}

/// The inner f.req array (length 69): only a handful of indices carry data, the rest are null.
/// `resume` is a prior `cid,rid,rcid` token; setting inner[2] to it continues that conversation.
fn build_inner(prompt: &str, hl: &str, uuid: &str, resume: Option<&str>) -> Value {
    let mut a = vec![Value::Null; 69];
    a[0] = json!([prompt, 0, null, null, null, null, 0]);
    a[1] = json!([hl]);
    a[2] = match resume {
        Some(meta) => {
            let ids: Vec<&str> = meta.split(',').collect();
            json!([
                ids.first().copied().unwrap_or(""),
                ids.get(1).copied().unwrap_or(""),
                ids.get(2).copied().unwrap_or(""),
            ])
        }
        None => json!(["", "", "", null, null, null, null, null, null, ""]),
    };
    a[6] = json!([1]);
    a[7] = json!(1);
    a[10] = json!(1);
    a[11] = json!(0);
    a[17] = json!([[0]]);
    a[18] = json!(0);
    a[27] = json!(1);
    a[30] = json!([4]);
    a[41] = json!([1]);
    a[53] = json!(0);
    a[59] = json!(uuid);
    a[61] = json!([]);
    a[68] = json!(2);
    Value::Array(a)
}

/// Per-turn request id: seeded low, advanced by 100000 each call (matches the web client).
fn reqid() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static R: AtomicU64 = AtomicU64::new(10000);
    R.fetch_add(100_000, Ordering::Relaxed)
}

/// Scrape a `"key":"value"` JS literal from the bootstrap HTML (tolerates whitespace after `:`).
fn scrape(html: &str, key: &str) -> Option<String> {
    let anchor = format!("\"{key}\":");
    let i = html.find(&anchor)? + anchor.len();
    let rest = html[i..].trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn form_encode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Parse the framed StreamGenerate response. Frames are cumulative snapshots, so keep the
/// longest answer (at `resp[4][0][1][0]`) and the latest conversation ids (cid,rid at
/// `resp[1]`; rcid at `resp[4][0][0]`) for follow-ups.
fn parse(body: &str) -> Result<Outcome> {
    let mut best = String::new();
    let mut meta: Option<String> = None;
    for frame in frames(body) {
        let outer: Value = match serde_json::from_str(&frame) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for item in outer.as_array().into_iter().flatten() {
            let it = match item.as_array() {
                Some(x) => x,
                None => continue,
            };
            if it.first().and_then(|x| x.as_str()) != Some("wrb.fr") {
                continue;
            }
            if usage_limited(it) {
                return Err(Error::RateLimit("gemini_web: usage limit exceeded".into()));
            }
            let resp: Value = match it.get(2).and_then(|x| x.as_str()) {
                Some(p) => match serde_json::from_str(p) {
                    Ok(v) => v,
                    Err(_) => continue,
                },
                None => continue,
            };
            if let Some(t) = resp
                .get(4)
                .and_then(|c| c.get(0))
                .and_then(|c| c.get(1))
                .and_then(|c| c.get(0))
                .and_then(|t| t.as_str())
            {
                if t.len() > best.len() {
                    best = t.to_string();
                }
            }
            let cid = resp.get(1).and_then(|m| m.get(0)).and_then(|x| x.as_str());
            let rid = resp.get(1).and_then(|m| m.get(1)).and_then(|x| x.as_str());
            if let (Some(c), Some(r)) = (cid, rid) {
                let rcid = resp
                    .get(4)
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get(0))
                    .and_then(|x| x.as_str())
                    .unwrap_or_default();
                meta = Some(format!("{c},{r},{rcid}"));
            }
        }
    }
    if best.is_empty() {
        return Err(Error::BadResponse("gemini_web"));
    }
    let mut out = Outcome::new(best, 1);
    out.session = meta;
    Ok(out)
}

/// Extract a deep-research PLAN (title, ETA, steps) + conversation ids. The plan lives at
/// `resp[4][0][12][0]["56"]`: [0]=title, [1]=steps tree, [2]=eta.
fn parse_plan(body: &str) -> Result<Outcome> {
    let mut plan: Option<String> = None;
    let mut meta: Option<String> = None;
    for frame in frames(body) {
        let outer: Value = match serde_json::from_str(&frame) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for item in outer.as_array().into_iter().flatten() {
            let it = match item.as_array() {
                Some(x) => x,
                None => continue,
            };
            if it.first().and_then(|x| x.as_str()) != Some("wrb.fr") {
                continue;
            }
            if usage_limited(it) {
                return Err(Error::RateLimit("gemini_web: usage limit exceeded".into()));
            }
            let resp: Value = match it.get(2).and_then(|x| x.as_str()) {
                Some(p) => match serde_json::from_str(p) {
                    Ok(v) => v,
                    Err(_) => continue,
                },
                None => continue,
            };
            if let Some(p) = resp
                .get(4)
                .and_then(|c| c.get(0))
                .and_then(|c| c.get(12))
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("56"))
            {
                let title = p.get(0).and_then(|x| x.as_str()).unwrap_or("Research plan");
                let eta = p.get(2).and_then(|x| x.as_str()).unwrap_or_default();
                let mut steps = Vec::new();
                if let Some(s) = p.get(1) {
                    collect_strings(s, &mut steps);
                }
                plan = Some(format!("# {title}\n_{eta}_\n\n{}", steps.join("\n")));
            }
            let cid = resp.get(1).and_then(|m| m.get(0)).and_then(|x| x.as_str());
            let rid = resp.get(1).and_then(|m| m.get(1)).and_then(|x| x.as_str());
            if let (Some(c), Some(r)) = (cid, rid) {
                let rcid = resp
                    .get(4)
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get(0))
                    .and_then(|x| x.as_str())
                    .unwrap_or_default();
                meta = Some(format!("{c},{r},{rcid}"));
            }
        }
    }
    let plan = plan.ok_or(Error::BadResponse(
        "gemini_web: no research plan (deep research may be unavailable on this account)",
    ))?;
    let mut out = Outcome::new(
        format!("{plan}\n\nReply with this session + query \"start\" to run the research, or send an adjustment to refine the plan."),
        2,
    );
    out.session = meta.map(|m| format!("dr|{m}"));
    Ok(out)
}

/// Recursively gather human-readable strings (dropping googleusercontent artifacts).
fn collect_strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => {
            if s.len() > 2 && !s.starts_with("http://googleusercontent.com") {
                out.push(s.clone());
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_strings(x, out)),
        Value::Object(o) => o.values().for_each(|x| collect_strings(x, out)),
        _ => {}
    }
}

/// Map a friendly model name to Gemini's opaque model id (or pass a raw id through).
/// ponytail: ids rotate with model launches; update this map or pass the raw id. Unknown
/// names fall back to the account default (no header).
fn model_id(m: Option<&str>) -> Option<String> {
    let m = m?;
    let k: String = m
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let id = match k.as_str() {
        "pro" | "3pro" | "31pro" | "gemini3pro" | "gemini31pro" => "9d8ca3786ebdfbea",
        "flash" | "3flash" | "35flash" | "gemini3flash" => "fbb127bbb056c959",
        _ if m.len() >= 12 && m.bytes().all(|b| b.is_ascii_hexdigit()) => {
            return Some(m.to_string())
        }
        _ => return None,
    };
    Some(id.to_string())
}

/// Gemini's only reliable "out of quota" signal: a fatal error code at `wrb.fr[5][2][0][1][0]`,
/// where 1037 = USAGE_LIMIT_EXCEEDED. The web app exposes no remaining-count, so this reactive
/// hit is all there is — map it to a rate-limit so the account is marked exhausted and the router
/// fails over. Index path mirrors HanaokaYuzu/Gemini-API's detection.
fn usage_limited(it: &[Value]) -> bool {
    it.get(5)
        .and_then(|x| x.get(2))
        .and_then(|x| x.get(0))
        .and_then(|x| x.get(1))
        .and_then(|x| x.get(0))
        .and_then(|x| x.as_i64())
        == Some(1037)
}

// Each payload is one compact-JSON array per physical line (Gemini escapes any newline
// inside strings), so split on lines and skip the `)]}'` prelude + bare length markers.
// ponytail: line-based instead of the documented UTF-16 length prefix, whose count desyncs
// the stream here; revisit only if a payload ever spans physical lines.
fn frames(s: &str) -> Vec<String> {
    s.lines()
        .map(str::trim)
        .filter(|l| l.starts_with("[["))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_framed_answer_with_multibyte() {
        let resp = json!([null, null, null, null, [[null, ["The answer 😀 inside"]]]]).to_string();
        let outer = json!([["wrb.fr", null, resp]]).to_string();
        let n = outer.encode_utf16().count();
        let body = format!(")]}}'\n{n}\n{outer}");
        let out = parse(&body).unwrap();
        assert!(out.text.contains("The answer"));
        assert!(out.text.contains('😀'));
    }

    #[test]
    fn scrape_reads_token() {
        let html = r#"...,"SNlM0e":"abc123","cfb2h":"build_42",..."#;
        assert_eq!(scrape(html, "SNlM0e").as_deref(), Some("abc123"));
        assert_eq!(scrape(html, "cfb2h").as_deref(), Some("build_42"));
    }

    #[test]
    fn usage_limit_maps_to_rate_limit() {
        // wrb.fr item carrying the fatal code at [5][2][0][1][0] = 1037 (USAGE_LIMIT_EXCEEDED).
        let item = json!([
            "wrb.fr",
            null,
            null,
            null,
            null,
            [null, null, [[null, [1037]]]],
            "generic"
        ]);
        let outer = json!([item]).to_string();
        let n = outer.encode_utf16().count();
        let body = format!(")]}}'\n{n}\n{outer}");
        assert!(matches!(parse(&body), Err(Error::RateLimit(_))));
    }
}

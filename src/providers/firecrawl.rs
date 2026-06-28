use serde_json::{json, Value};

use super::{check, fmt_hits, s, Capability, Hit, Input, Outcome};
use crate::error::{Error, Result};

pub async fn call(
    base: &str,
    key: &str,
    client: &reqwest::Client,
    cap: Capability,
    input: &Input,
) -> Result<Outcome> {
    match cap {
        Capability::Search => search(base, key, client, input).await,
        Capability::Read => scrape(base, key, client, input).await,
        _ => Err(Error::Unsupported("firecrawl")),
    }
}

async fn search(base: &str, key: &str, client: &reqwest::Client, input: &Input) -> Result<Outcome> {
    let resp = client
        .post(format!("{base}/v1/search"))
        .bearer_auth(key)
        .json(&json!({ "query": input.need_query()?, "limit": input.results() }))
        .send()
        .await?;
    let v: Value = check("firecrawl", resp).await?.json().await?;
    let hits = v["data"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|o| Hit {
                    title: s(o, "title"),
                    url: s(o, "url"),
                    snippet: s(o, "description"),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(Outcome::new(fmt_hits(&hits), 1))
}

async fn scrape(base: &str, key: &str, client: &reqwest::Client, input: &Input) -> Result<Outcome> {
    let resp = client
        .post(format!("{base}/v1/scrape"))
        .bearer_auth(key)
        .json(&json!({ "url": input.need_url()?, "formats": ["markdown"] }))
        .send()
        .await?;
    let v: Value = check("firecrawl", resp).await?.json().await?;
    let text = v["data"]["markdown"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or(Error::BadResponse("firecrawl"))?
        .to_string();
    let cost = v["data"]["metadata"]["creditsUsed"].as_i64().unwrap_or(1);
    Ok(Outcome::new(text, cost))
}

use std::time::Duration;

use serde_json::{json, Value};

use super::{check, fmt_hits, s, value_to_text, Capability, Hit, Input, Outcome};
use crate::error::{Error, Result};

const POLL: Duration = Duration::from_secs(3);
const MAX_POLLS: u32 = 40;

pub async fn call(
    base: &str,
    key: &str,
    client: &reqwest::Client,
    cap: Capability,
    input: &Input,
) -> Result<Outcome> {
    match cap {
        Capability::Search => search(base, key, client, input).await,
        Capability::DeepResearch => research(base, key, client, input).await,
        _ => Err(Error::Unsupported("parallel")),
    }
}

async fn search(base: &str, key: &str, client: &reqwest::Client, input: &Input) -> Result<Outcome> {
    let q = input.need_query()?;
    let resp = client
        .post(format!("{base}/v1beta/search"))
        .header("x-api-key", key)
        .json(&json!({
            "objective": q,
            "search_queries": [q],
            "max_results": input.results(),
        }))
        .send()
        .await?;
    let v: Value = check("parallel", resp).await?.json().await?;
    let hits = v["results"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|o| Hit {
                    title: s(o, "title"),
                    url: s(o, "url"),
                    snippet: o["excerpts"]
                        .as_array()
                        .map(|e| {
                            e.iter()
                                .filter_map(|x| x.as_str())
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .unwrap_or_default(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(Outcome::new(fmt_hits(&hits), 1))
}

async fn research(
    base: &str,
    key: &str,
    client: &reqwest::Client,
    input: &Input,
) -> Result<Outcome> {
    let start = client
        .post(format!("{base}/v1/tasks/runs"))
        .header("x-api-key", key)
        .json(&json!({ "input": input.need_query()?, "processor": "base" }))
        .send()
        .await?;
    let v: Value = check("parallel", start).await?.json().await?;
    let id = v["run_id"]
        .as_str()
        .ok_or(Error::BadResponse("parallel"))?
        .to_string();

    for _ in 0..MAX_POLLS {
        tokio::time::sleep(POLL).await;
        let resp = client
            .get(format!("{base}/v1/tasks/runs/{id}"))
            .header("x-api-key", key)
            .send()
            .await?;
        let v: Value = check("parallel", resp).await?.json().await?;
        match v["status"].as_str().unwrap_or_default() {
            "completed" => return result(base, key, client, &id).await,
            "failed" | "cancelled" | "canceled" => return Err(Error::BadResponse("parallel")),
            _ => continue,
        }
    }
    Err(Error::Timeout("parallel"))
}

async fn result(base: &str, key: &str, client: &reqwest::Client, id: &str) -> Result<Outcome> {
    let resp = client
        .get(format!("{base}/v1/tasks/runs/{id}/result"))
        .header("x-api-key", key)
        .send()
        .await?;
    let v: Value = check("parallel", resp).await?.json().await?;
    let content = &v["output"]["content"];
    let text = content["output"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| value_to_text(content));
    Ok(Outcome::new(text, 1))
}

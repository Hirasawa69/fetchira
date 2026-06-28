use serde_json::{json, Value};

use super::{check, fmt_hits, s, Capability, Hit, Input, Outcome};
use crate::error::Result;

pub async fn call(
    base: &str,
    key: &str,
    client: &reqwest::Client,
    _cap: Capability,
    input: &Input,
) -> Result<Outcome> {
    let q = input.need_query()?;
    let resp = client
        .post(format!("{base}/search"))
        .header("X-API-KEY", key)
        .json(&json!({ "q": q, "num": input.results() }))
        .send()
        .await?;
    let v: Value = check("serper", resp).await?.json().await?;
    let hits = v["organic"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|o| Hit {
                    title: s(o, "title"),
                    url: s(o, "link"),
                    snippet: s(o, "snippet"),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(Outcome::new(fmt_hits(&hits), 1))
}

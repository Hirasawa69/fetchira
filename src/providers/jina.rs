use serde_json::Value;

use super::{check, Capability, Input, Outcome};
use crate::error::{Error, Result};

pub async fn call(
    base: &str,
    key: &str,
    client: &reqwest::Client,
    _cap: Capability,
    input: &Input,
) -> Result<Outcome> {
    let url = input.need_url()?;
    let resp = client
        .get(format!("{base}/{url}"))
        .bearer_auth(key)
        .header("Accept", "application/json")
        .send()
        .await?;
    let v: Value = check("jina", resp).await?.json().await?;
    let text = v["data"]["content"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or(Error::BadResponse("jina"))?
        .to_string();
    Ok(Outcome::new(text, 1))
}

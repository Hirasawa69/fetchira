use serde_json::{json, Value};

use super::{check, Capability, Input, Outcome};
use crate::error::{Error, Result};

// ponytail: v1 drives Steel's REST /v1/scrape only (clean markdown of one page).
// Multi-step CDP/Playwright automation (the `actions` arg) is deferred to v2; session
// time is approximated as a fixed per-call cost since /v1/scrape returns no duration.
const SCRAPE_SECS: i64 = 30;

pub async fn call(
    base: &str,
    key: &str,
    client: &reqwest::Client,
    _cap: Capability,
    input: &Input,
) -> Result<Outcome> {
    let resp = client
        .post(format!("{base}/v1/scrape"))
        .header("steel-api-key", key)
        .json(&json!({ "url": input.need_url()?, "format": ["markdown"] }))
        .send()
        .await?;
    let v: Value = check("steel", resp).await?.json().await?;
    let text = v["content"]["markdown"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or(Error::BadResponse("steel"))?
        .to_string();
    Ok(Outcome::new(text, SCRAPE_SECS))
}

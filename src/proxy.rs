use std::time::Duration;

use crate::config::ProxyPool;
use crate::error::Result;

/// Resolve the proxy pool to a list of `http://user:pass@ip:port` URLs.
/// Explicit `proxies` win; otherwise download the Webshare list.
pub async fn resolve_pool(pool: &ProxyPool, client: &reqwest::Client) -> Result<Vec<String>> {
    if !pool.proxies.is_empty() {
        return Ok(pool.proxies.clone());
    }
    let Some(url) = &pool.webshare_url else {
        return Ok(Vec::new());
    };
    let body = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(body.lines().filter_map(parse_line).collect())
}

/// Webshare default line: `ip:port:username:password` (password may contain `:`).
fn parse_line(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let mut it = line.splitn(4, ':');
    let ip = it.next()?;
    let port = it.next()?;
    let user = it.next()?;
    let pass = it.next()?;
    Some(format!("http://{user}:{pass}@{ip}:{port}"))
}

/// One reqwest client per account: proxy is a client-level setting. Userinfo in the
/// URL is split out into explicit basic auth, since proxies are HTTP-CONNECT (`http://`).
pub fn build_client(proxy: Option<&str>) -> Result<reqwest::Client> {
    let mut b = reqwest::Client::builder().timeout(Duration::from_secs(60));
    if let Some(p) = proxy {
        let (url, auth) = split_auth(p);
        let mut px = reqwest::Proxy::all(&url)?;
        if let Some((u, pw)) = auth {
            px = px.basic_auth(&u, &pw);
        }
        b = b.proxy(px);
    }
    Ok(b.build()?)
}

pub(crate) fn split_auth(p: &str) -> (String, Option<(String, String)>) {
    let (scheme, rest) = p.split_once("://").unwrap_or(("http", p));
    if let Some((cred, host)) = rest.rsplit_once('@') {
        if let Some((u, pw)) = cred.split_once(':') {
            return (
                format!("{scheme}://{host}"),
                Some((u.to_string(), pw.to_string())),
            );
        }
    }
    (p.to_string(), None)
}

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use wreq::cookie::Jar;
use wreq::Url;
use wreq_util::Emulation;

use crate::error::{Error, Result};
use crate::providers::ProviderKind;
use crate::proxy::split_auth;

/// A captured browser cookie. Field names match Chrome DevTools (camelCase) so the same
/// shape round-trips through CDP capture and the stored session JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    #[serde(default = "slash")]
    pub path: String,
    #[serde(default)]
    pub expires: f64,
    #[serde(default)]
    pub http_only: bool,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub session: bool,
}

fn slash() -> String {
    "/".into()
}

/// A captured web session: cookies plus any extra default headers to send with them. (grok's
/// `x-statsig-id` is not a cookie — it's generated per request in `providers::grok_web`.)
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Session {
    pub cookies: Vec<Cookie>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

/// Parse a stored session, accepting both the new `{cookies, headers}` object and the
/// original bare cookie array.
pub fn parse_session(raw: &str) -> Session {
    serde_json::from_str::<Session>(raw)
        .or_else(|_| {
            serde_json::from_str::<Vec<Cookie>>(raw).map(|cookies| Session {
                cookies,
                headers: BTreeMap::new(),
            })
        })
        .unwrap_or_default()
}

/// Build a Chrome-impersonating client with the captured cookies + headers (and optional
/// sticky proxy) baked in. The long timeout covers deep-research turns that run for minutes.
pub fn build_client(
    cookies: &[Cookie],
    headers: &BTreeMap<String, String>,
    proxy: Option<&str>,
) -> Result<wreq::Client> {
    let jar = Jar::default();
    for c in cookies {
        // Respect cookie-prefix rules or cookie_store silently drops these: `__Secure-`
        // requires Secure; `__Host-` requires Secure + Path=/ + no Domain.
        let host_only = c.name.starts_with("__Host-");
        let mut s = format!("{}={}; Path={}", c.name, c.value, c.path);
        if !host_only {
            s.push_str(&format!("; Domain={}", c.domain));
        }
        if c.secure || host_only || c.name.starts_with("__Secure-") {
            s.push_str("; Secure");
        }
        let site = format!("https://{}/", c.domain.trim_start_matches('.'));
        if let Ok(url) = site.parse::<Url>() {
            jar.add_cookie_str(&s, &url);
        }
    }
    let mut b = wreq::Client::builder()
        .emulation(Emulation::Chrome137)
        .cookie_provider(Arc::new(jar))
        .timeout(Duration::from_secs(300));
    if !headers.is_empty() {
        let mut hmap = wreq::header::HeaderMap::new();
        for (k, v) in headers {
            if let (Ok(name), Ok(val)) = (
                wreq::header::HeaderName::from_bytes(k.as_bytes()),
                wreq::header::HeaderValue::from_str(v),
            ) {
                hmap.insert(name, val);
            }
        }
        b = b.default_headers(hmap);
    }
    if let Some(p) = proxy {
        let (url, auth) = split_auth(p);
        let mut px = wreq::Proxy::all(&url)?;
        if let Some((u, pw)) = auth {
            px = px.basic_auth(&u, &pw);
        }
        b = b.proxy(px);
    }
    Ok(b.build()?)
}

const CHROME_PATHS: &[&str] = &[
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
];

fn chrome_bin() -> Option<&'static str> {
    CHROME_PATHS
        .iter()
        .copied()
        .find(|p| std::path::Path::new(p).exists())
}

/// (login URL, registrable domain, auth-cookie name) per web provider.
fn login_target(kind: ProviderKind) -> Result<(&'static str, &'static str, &'static str)> {
    Ok(match kind {
        ProviderKind::GeminiWeb => ("https://gemini.google.com/", "google.com", "__Secure-1PSID"),
        ProviderKind::PerplexityWeb => (
            "https://www.perplexity.ai/",
            "perplexity.ai",
            "__Secure-next-auth.session-token",
        ),
        ProviderKind::GrokWeb => ("https://grok.com/", "grok.com", "sso"),
        other => return Err(Error::Unsupported(other.as_str())),
    })
}

/// One Chrome profile per account label, so multiple accounts of the same provider can each be
/// logged into a different account (e.g. gemini-1 and gemini-2 as two different Google users).
fn profile_dir(label: &str) -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join("Library/Application Support/fetchira")
        .join(format!("chrome-{label}"))
}

/// Launch a real Chrome on this account's dedicated profile, let the user log in, and capture
/// the resulting session (cookies + any needed headers) over CDP once auth completes.
pub async fn login(kind: ProviderKind, label: &str) -> Result<Session> {
    let bin = chrome_bin().ok_or_else(|| Error::Config("no Chrome/Chromium found".into()))?;
    let (url, domain, auth) = login_target(kind)?;
    let port = 9222u16;

    let mut child = tokio::process::Command::new(bin)
        .arg(format!("--user-data-dir={}", profile_dir(label).display()))
        .arg(format!("--remote-debugging-port={port}"))
        .arg("--remote-allow-origins=*")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-logging")
        .arg("--log-level=3")
        .arg(format!("--app={url}"))
        // Chrome (and the GoogleUpdater it spawns) is noisy on stderr — keep it off the terminal.
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    let session = timeout(Duration::from_secs(300), capture(port, domain, auth))
        .await
        .map_err(|_| Error::Timeout("login"))?;
    let _ = child.kill().await;
    session
}

async fn capture(port: u16, domain: &str, auth: &str) -> Result<Session> {
    let ws_url = wait_for_page(port).await?;
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url.as_str()).await?;
    send_cmd(&mut ws, 1, "Network.enable", Value::Null).await?;

    let mut id = 1u64;
    // Phase 1: wait until the provider's auth cookie shows up.
    let mut best = loop {
        let scoped = scoped_cookies(&mut ws, &mut id, domain).await?;
        if scoped.iter().any(|c| is_auth(c, auth)) {
            break scoped;
        }
        sleep(Duration::from_secs(1)).await;
    };
    // Phase 2: the auth cookie appears before its companions (e.g. Google sets
    // __Secure-1PSIDTS a beat later via Set-Cookie). Keep the fullest set until it
    // stops growing for two polls, capped at ~8s.
    let mut stable = 0;
    for _ in 0..8 {
        sleep(Duration::from_secs(1)).await;
        let scoped = scoped_cookies(&mut ws, &mut id, domain).await?;
        if scoped.len() > best.len() {
            best = scoped;
            stable = 0;
        } else {
            stable += 1;
            if stable >= 2 {
                break;
            }
        }
    }
    Ok(Session {
        cookies: best,
        headers: BTreeMap::new(),
    })
}

async fn scoped_cookies(ws: &mut Ws, id: &mut u64, domain: &str) -> Result<Vec<Cookie>> {
    *id += 1;
    let res = send_cmd(ws, *id, "Network.getAllCookies", Value::Null).await?;
    let all: Vec<Cookie> = serde_json::from_value(res["cookies"].clone()).unwrap_or_default();
    Ok(all
        .into_iter()
        .filter(|c| dom_match(&c.domain, domain))
        .collect())
}

fn is_auth(c: &Cookie, auth: &str) -> bool {
    c.name == auth && !c.value.is_empty() && (c.session || c.expires <= 0.0 || c.expires > now())
}

/// Poll the DevTools HTTP endpoint for a page target and return its WebSocket URL.
async fn wait_for_page(port: u16) -> Result<String> {
    let http = reqwest::Client::new();
    for _ in 0..60 {
        if let Ok(resp) = http
            .get(format!("http://127.0.0.1:{port}/json"))
            .send()
            .await
        {
            if let Ok(targets) = resp.json::<Vec<Value>>().await {
                if let Some(ws) = targets.iter().find_map(|t| {
                    (t["type"] == "page")
                        .then(|| t["webSocketDebuggerUrl"].as_str())
                        .flatten()
                }) {
                    return Ok(ws.to_string());
                }
            }
        }
        sleep(Duration::from_millis(500)).await;
    }
    Err(Error::Timeout("chrome devtools"))
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Send one CDP command and read frames until the matching id; events (no id) are skipped.
async fn send_cmd(ws: &mut Ws, id: u64, method: &str, params: Value) -> Result<Value> {
    let mut req = json!({ "id": id, "method": method });
    if !params.is_null() {
        req["params"] = params;
    }
    ws.send(Message::Text(req.to_string().into())).await?;
    while let Some(frame) = ws.next().await {
        if let Message::Text(txt) = frame? {
            let msg: Value = serde_json::from_str(txt.as_str())?;
            if msg["id"].as_u64() == Some(id) {
                return Ok(msg["result"].clone());
            }
        }
    }
    Err(Error::BadResponse("cdp connection closed"))
}

fn dom_match(cookie_domain: &str, want: &str) -> bool {
    let c = cookie_domain.trim_start_matches('.');
    c == want || c.ends_with(&format!(".{want}"))
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

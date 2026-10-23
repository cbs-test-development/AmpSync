//! Recording HTTP layer. Every request is logged with method, path TEMPLATE (never the real
//! URL), status, timing and a few headers. Cookie values are never logged.

use crate::auth::Captured;
use crate::redact::{scrub_text, url_shape};
use anyhow::{anyhow, Result};
use reqwest::header::{HeaderMap, LOCATION};
use reqwest::{cookie::Jar, Client, Method};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use url::Url;

pub const UA: &str = "NONISV|AmpSync|AmpSync-probe/0.1";

#[derive(Serialize, Clone, Debug)]
pub struct HttpRec {
    pub exp: String,
    pub method: String,
    pub path_template: String,
    pub status: Option<u16>,
    pub elapsed_ms: u64,
    pub bytes: u64,
    pub headers: BTreeMap<String, String>,
    pub note: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Hop {
    pub status: u16,
    pub to: String,
}

pub struct Req {
    pub method: Method,
    pub url: String,
    pub template: String,
    pub exp: &'static str,
    pub headers: Vec<(String, String)>,
    pub cookies: bool,
    pub follow: bool,
    pub max_body: usize,
    pub store: bool,
    pub body: Option<Vec<u8>>,
}

impl Req {
    pub fn get(exp: &'static str, template: &str, url: String) -> Self {
        Req {
            method: Method::GET,
            url,
            template: template.to_string(),
            exp,
            headers: vec![("Accept-Encoding".into(), "identity".into())],
            cookies: true,
            follow: false,
            max_body: 16 * 1024 * 1024,
            store: true,
            body: None,
        }
    }
    /// A POST that carries cookies by default (same recording client; cookies are sent for POSTs
    /// exactly as they are for GETs). Used only for the two read-only, state-changing-nothing
    /// calls E8 makes (`contextinfo`, `SP.OAuth.Token/Acquire`).
    pub fn post(exp: &'static str, template: &str, url: String, body: Vec<u8>) -> Self {
        Req {
            method: Method::POST,
            url,
            template: template.to_string(),
            exp,
            headers: vec![("Accept-Encoding".into(), "identity".into())],
            cookies: true,
            follow: false,
            max_body: 1024 * 1024,
            store: true,
            body: Some(body),
        }
    }
    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.to_string(), v.to_string()));
        self
    }
    pub fn json(self) -> Self {
        self.header("Accept", "application/json")
    }
    pub fn nometadata(self) -> Self {
        self.header("Accept", "application/json;odata=nometadata")
    }
    pub fn no_cookies(mut self) -> Self {
        self.cookies = false;
        self
    }
    pub fn follow(mut self) -> Self {
        self.follow = true;
        self
    }
    pub fn max_body(mut self, n: usize) -> Self {
        self.max_body = n;
        self
    }
    pub fn no_store(mut self) -> Self {
        self.store = false;
        self
    }
    pub fn range(self, start: u64, end_inclusive: u64) -> Self {
        let v = format!("bytes={start}-{end_inclusive}");
        self.header("Range", &v)
    }
}

pub struct Resp {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    pub received: u64,
    /// Body was cut at `max_body` (server sent more than asked for).
    pub over_limit: bool,
    pub final_url: Url,
    pub hops: Vec<Hop>,
}

impl Resp {
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers.get(name).and_then(|v| v.to_str().ok()).map(|s| s.to_string())
    }
    pub fn json(&self) -> Result<Value> {
        serde_json::from_slice(&self.body).map_err(|e| anyhow!("response is not JSON ({e}); status {}", self.status))
    }
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

pub struct Http {
    client: Client,
    bare: Client,
    pub log: Mutex<Vec<HttpRec>>,
}

fn is_redirect(s: u16) -> bool {
    matches!(s, 301 | 302 | 303 | 307 | 308)
}

impl Http {
    pub fn new(cookies: &[Captured], host: &str) -> Result<Self> {
        let jar = Arc::new(Jar::default());
        let base = Url::parse(&format!("{}://{host}/", crate::urlparse::scheme()))?;
        let secure = if crate::urlparse::scheme() == "https" { "; Secure" } else { "" };
        for c in cookies {
            // Host-only cookie on the tenant host (no Domain attribute) so it is never sent elsewhere.
            jar.add_cookie_str(&format!("{}={}; Path=/{secure}", c.name, c.value), &base);
        }
        let mk = |provider: Option<Arc<Jar>>| -> Result<Client> {
            let mut b = Client::builder().user_agent(UA).redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(180)).connect_timeout(Duration::from_secs(20));
            if let Some(j) = provider {
                b = b.cookie_provider(j);
            }
            Ok(b.build()?)
        };
        Ok(Http { client: mk(Some(jar))?, bare: mk(None)?, log: Mutex::new(Vec::new()) })
    }

    fn record(&self, rec: HttpRec) {
        self.log.lock().unwrap().push(rec);
    }

    pub fn snapshot_len(&self) -> usize {
        self.log.lock().unwrap().len()
    }

    pub async fn send(&self, r: Req) -> Result<Resp> {
        let mut url = Url::parse(&r.url).map_err(|e| anyhow!("bad url: {e}"))?;
        let mut hops: Vec<Hop> = Vec::new();
        for hop_no in 0..=10 {
            let client = if r.cookies { &self.client } else { &self.bare };
            let mut rb = client.request(r.method.clone(), url.clone());
            for (k, v) in &r.headers {
                rb = rb.header(k.as_str(), v.as_str());
            }
            if let Some(b) = &r.body {
                rb = rb.body(b.clone());
            }
            let started = Instant::now();
            let template = if hop_no == 0 { r.template.clone() } else { format!("{} (redirect hop {hop_no})", r.template) };
            let mut res = match rb.send().await {
                Ok(x) => x,
                Err(e) => {
                    let msg = scrub_text(&e.without_url().to_string());
                    self.record(HttpRec {
                        exp: r.exp.into(),
                        method: r.method.to_string(),
                        path_template: template,
                        status: None,
                        elapsed_ms: started.elapsed().as_millis() as u64,
                        bytes: 0,
                        headers: BTreeMap::new(),
                        note: Some(msg.clone()),
                    });
                    return Err(anyhow!("request failed: {msg}"));
                }
            };
            let status = res.status().as_u16();
            let headers = res.headers().clone();
            let mut hmap = BTreeMap::new();
            for k in ["content-type", "retry-after", "accept-ranges", "content-range", "content-length"] {
                if let Some(v) = headers.get(k).and_then(|v| v.to_str().ok()) {
                    hmap.insert(k.to_string(), v.to_string());
                }
            }
            if let Some(v) = headers.get("www-authenticate").and_then(|v| v.to_str().ok()) {
                hmap.insert("www-authenticate".into(), v.split_whitespace().next().unwrap_or("").to_string());
            }

            if r.follow && is_redirect(status) {
                let loc = headers.get(LOCATION).and_then(|v| v.to_str().ok()).ok_or_else(|| anyhow!("redirect without Location"))?;
                let next = url.join(loc).map_err(|e| anyhow!("bad redirect target: {e}"))?;
                hops.push(Hop { status, to: url_shape(&next) });
                self.record(HttpRec {
                    exp: r.exp.into(),
                    method: r.method.to_string(),
                    path_template: template,
                    status: Some(status),
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    bytes: 0,
                    headers: hmap,
                    note: Some(format!("-> {}", url_shape(&next))),
                });
                url = next;
                continue;
            }

            let mut body = Vec::new();
            let mut received = 0u64;
            let mut over = false;
            let mut note = None;
            loop {
                match res.chunk().await {
                    Ok(Some(c)) => {
                        received += c.len() as u64;
                        if r.store {
                            body.extend_from_slice(&c);
                        }
                        if received as usize > r.max_body {
                            over = true;
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        note = Some(format!("body read error: {}", scrub_text(&e.without_url().to_string())));
                        break;
                    }
                }
            }
            if r.store && body.len() > r.max_body {
                body.truncate(r.max_body);
            }
            self.record(HttpRec {
                exp: r.exp.into(),
                method: r.method.to_string(),
                path_template: template,
                status: Some(status),
                elapsed_ms: started.elapsed().as_millis() as u64,
                bytes: received,
                headers: hmap,
                note,
            });
            return Ok(Resp { status, headers, body, received, over_limit: over, final_url: url, hops });
        }
        Err(anyhow!("too many redirects"))
    }
}

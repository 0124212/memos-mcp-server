//! Minimal Memos API v1 HTTP client (reqwest + bearer token).
//!
//! Mirrors the official Go MCP adapter's REST surface: each curated
//! operation maps to one method+path pair taken from
//! `proto/gen/openapi.yaml` (see catalog.rs).

use anyhow::{Context, Result};
use reqwest::{Client, Method};
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct MemosClient {
    http: Client,
    base_url: String,
    token: String,
}

impl MemosClient {
    pub fn new(base_url: String, token: String) -> Self {
        let http = Client::builder()
            .user_agent("memos-mcp-server/0.1.0")
            .build()
            .expect("reqwest client builds");
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub async fn request(
        &self,
        method: Method,
        path: &str,
        query: Vec<(String, String)>,
        body: Option<Value>,
    ) -> Result<Value> {
        let mut req = self.http.request(method.clone(), self.url(path));
        if !self.token.is_empty() {
            req = req.bearer_auth(&self.token);
        }
        if !query.is_empty() {
            req = req.query(&query);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.context("memos request failed")?;
        let status = resp.status();
        let bytes = resp.bytes().await.context("read memos response")?;
        let value: Value = if bytes.trim_ascii().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).context("decode memos JSON response")?
        };
        if !status.is_success() {
            let msg = value
                .get("message")
                .and_then(|m| m.as_str())
                .or_else(|| value.get("error").and_then(|m| m.as_str()))
                .unwrap_or(status.canonical_reason().unwrap_or("error"));
            anyhow::bail!("{} {}: {}", status.as_u16(), status, msg);
        }
        Ok(value)
    }

    pub async fn get(&self, path: &str, query: Vec<(String, String)>) -> Result<Value> {
        self.request(Method::GET, path, query, None).await
    }

    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.request(Method::POST, path, vec![], Some(body)).await
    }

    pub async fn patch(&self, path: &str, body: Value) -> Result<Value> {
        self.request(Method::PATCH, path, vec![], Some(body)).await
    }

    pub async fn patch_q(
        &self,
        path: &str,
        query: Vec<(String, String)>,
        body: Value,
    ) -> Result<Value> {
        self.request(Method::PATCH, path, query, Some(body)).await
    }

    pub async fn delete(&self, path: &str) -> Result<Value> {
        self.request(Method::DELETE, path, vec![], None).await
    }
}

/// Accept `memos/abc123` or bare `abc123`; strip to the bare id for path params.
pub fn bare_id(value: &str) -> String {
    value
        .rsplit('/')
        .next()
        .unwrap_or(value)
        .trim()
        .to_string()
}

pub fn opt_query(params: Vec<(&str, Option<String>)>) -> Vec<(String, String)> {
    params
        .into_iter()
        .filter_map(|(k, v)| v.filter(|s| !s.is_empty()).map(|s| (k.to_string(), s)))
        .collect()
}

//! memos-mcp-server: standalone Rust MCP server for Memos.
//!
//! Transports:
//! - Streamable HTTP (default, stateless): `POST/GET/DELETE /mcp`, plus
//!   `/mcp/readonly`, `/mcp/x/{toolsets}`, `/mcp/x/{toolsets}/readonly`.
//!   Per-request `X-MCP-Readonly`, `X-MCP-Toolsets`, `X-MCP-Tools` and
//!   `X-MCP-Exclude-Tools` headers narrow the catalog further. Because the
//!   server is stateless, each request builds a freshly filtered service and
//!   forwards the raw HTTP request to `StreamableHttpService::handle`.
//! - stdio: `--stdio` flag (or `MEMOS_MCP_STDIO=1`).
//!
//! Config via env: `MEMOS_URL` (default `http://localhost:5230`),
//! `MEMOS_TOKEN` (or `MEMOS_ACCESS_TOKEN`), `BIND` (default
//! `127.0.0.1:8080`), `MEMOS_INSTANCE_URL` (origin allowlist).

mod catalog;
mod filter;
mod memos;
mod origin;
mod server;

use std::net::SocketAddr;

use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::any,
};
use http_body_util::{BodyExt, Full};
use rmcp::transport::{
    io::stdio,
    streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use tracing_subscriber::EnvFilter;

use crate::{
    filter::{FilteredServer, ToolFilter},
    origin::is_allowed_origin,
    server::MemosServer,
};

#[derive(Clone)]
struct AppState {
    base_url: String,
    token: String,
}

fn env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

async fn origin_guard(headers: HeaderMap, request: Request, next: Next) -> Response {
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let origin = headers
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let instance_url = std::env::var("MEMOS_INSTANCE_URL").unwrap_or_default();
    if !is_allowed_origin(&host, origin.as_deref(), &instance_url) {
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(Body::empty())
            .unwrap();
    }
    next.run(request).await
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let base_url = env("MEMOS_URL", "http://localhost:5230");
    let token = std::env::var("MEMOS_TOKEN")
        .or_else(|_| std::env::var("MEMOS_ACCESS_TOKEN"))
        .unwrap_or_default();
    if token.is_empty() {
        tracing::warn!("no MEMOS_TOKEN set; requests will be unauthenticated");
    }

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--stdio") || std::env::var("MEMOS_MCP_STDIO").is_ok() {
        return run_stdio(base_url, token).await;
    }

    let bind_str = env("BIND", "127.0.0.1:8080");
    let bind: SocketAddr = if bind_str.parse::<u16>().is_ok() {
        format!("0.0.0.0:{bind_str}").parse()?
    } else {
        bind_str.parse()?
    };
    run_http(base_url, token, bind).await
}

async fn run_stdio(base_url: String, token: String) -> anyhow::Result<()> {
    let service = MemosServer::new(base_url, token);
    let transport = stdio();
    let running = rmcp::serve_server(service, transport).await?;
    tracing::info!("memos MCP server on stdio");
    running.waiting().await?;
    Ok(())
}

fn http_service(
    state: &AppState,
    filter: ToolFilter,
) -> StreamableHttpService<FilteredServer, LocalSessionManager> {
    let state = state.clone();
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None);
    StreamableHttpService::new(
        move || {
            Ok(FilteredServer::new(
                MemosServer::new(state.base_url.clone(), state.token.clone()),
                &filter,
            ))
        },
        Default::default(),
        config,
    )
}

/// Single dispatch point for all `/mcp*` routes: derive the tool filter
/// from the request path (route aliases) plus `X-MCP-*` headers, then
/// forward the raw request to a freshly built filtered service.
async fn dispatch(State(state): State<AppState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let path = parts.uri.path().to_string();
    let mut filter = ToolFilter::from_headers(&parts.headers).with_path(&path);
    // Bare `/mcp/readonly` handled by with_path; keep explicit alias cheap.
    if path == "/mcp/readonly" {
        filter.readonly = true;
    }
    let bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::empty())
                .unwrap();
        }
    };
    let service = http_service(&state, filter);
    let hreq = http::Request::from_parts(parts, Full::new(bytes));
    let hresp = service.handle(hreq).await;
    let (mut parts, body) = hresp.into_parts();
    let bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => {
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::empty())
                .unwrap();
        }
    };
    parts.headers.remove("content-length");
    Response::from_parts(parts, Body::from(bytes))
}

async fn run_http(base_url: String, token: String, bind: SocketAddr) -> anyhow::Result<()> {
    let state = AppState { base_url, token };
    let app = Router::new()
        .route("/mcp", any(dispatch))
        .route("/mcp/readonly", any(dispatch))
        .route("/mcp/x/{toolsets}", any(dispatch))
        .route("/mcp/x/{toolsets}/readonly", any(dispatch))
        .with_state(state)
        .layer(middleware::from_fn(origin_guard));

    tracing::info!("memos MCP server on http://{bind}/mcp");
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

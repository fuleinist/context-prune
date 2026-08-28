//! OpenAI-compatible reverse proxy (F1) with request/response compression (F2).
//!
//! Safety rule (F5): any compression failure falls back to the original bytes.
//! The proxy never returns 5xx because of a compression bug.

use axum::{
    body::Body,
    extract::State,
    http::{header, Request, Uri},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::cache::{self, CacheStore};
use crate::compress::{self, CompressConfig};
use crate::profiles::{self, Profile};
use crate::stats::StatsStore;

#[derive(Clone)]
pub struct ProxyState {
    pub upstream: String,
    pub passthrough: bool,
    pub profile: Profile,
    pub cache: Option<Arc<Mutex<CacheStore>>>,
    pub cache_hits: Arc<AtomicU64>,
    pub client: reqwest::Client,
    pub stats: Arc<Mutex<StatsStore>>,
}

pub async fn serve(
    port: u16,
    upstream: &str,
    profile_name: &str,
    min_size_override: Option<usize>,
    db: &str,
    passthrough: bool,
    no_cache: bool,
) -> anyhow::Result<()> {
    let mut profile = profiles::resolve(profile_name)
        .ok_or_else(|| anyhow::anyhow!("unknown profile '{profile_name}' (try: default, conservative, aggressive)"))?;
    if let Some(ms) = min_size_override {
        profile.config.min_size = ms;
    }
    let stats = Arc::new(Mutex::new(StatsStore::open(db)?));
    let cache = if no_cache {
        None
    } else {
        Some(Arc::new(Mutex::new(CacheStore::open(db)?)))
    };
    let state = ProxyState {
        upstream: upstream.trim_end_matches('/').to_string(),
        passthrough,
        profile,
        cache,
        cache_hits: Arc::new(AtomicU64::new(0)),
        client: reqwest::Client::new(),
        stats,
    };

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    eprintln!(
        "context-prune listening on http://{addr} -> {} (profile: {})",
        state.upstream, state.profile.name
    );

    let app = Router::new()
        .route("/stats", axum::routing::get(stats_handler))
        .route("/{*path}", any(proxy_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn stats_handler(State(state): State<ProxyState>) -> impl IntoResponse {
    let summary = match state.stats.lock().unwrap().summary() {
        Ok(s) => s,
        Err(e) => return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let cache_hits = state.cache_hits.load(Ordering::Relaxed);
    let cache_entries = state.cache.as_ref()
        .and_then(|c| c.lock().unwrap().entries().ok())
        .unwrap_or(0);
    let saved = summary.bytes_in.saturating_sub(summary.bytes_out);
    let ratio = if summary.bytes_in > 0 {
        (saved as f64 / summary.bytes_in as f64) * 100.0
    } else {
        0.0
    };
    axum::Json(serde_json::json!({
        "requests": summary.requests,
        "bytes_in": summary.bytes_in,
        "bytes_out": summary.bytes_out,
        "bytes_saved": saved,
        "savings_percent": (ratio * 10.0).round() / 10.0,
        "cache_hits": cache_hits,
        "cache_entries": cache_entries,
    }))
    .into_response()
}

async fn proxy_handler(State(state): State<ProxyState>, req: Request<Body>) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    // Build upstream URL.
    let upstream_url = match format!("{}{}", state.upstream, req.uri().path_and_query().map(|pq| pq.as_str()).unwrap_or(&path)).parse::<Uri>() {
        Ok(u) => u,
        Err(e) => {
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                format!("bad upstream url: {e}"),
            )
                .into_response()
        }
    };

    // Forward headers (except host/connection).
    let mut upstream_req = state.client.request(method.clone(), upstream_url.to_string());
    for (name, value) in req.headers() {
        if matches!(
            name.as_str(),
            "host" | "connection" | "content-length" | "accept-encoding"
        ) {
            continue;
        }
        if let Ok(v) = value.to_str() {
            upstream_req = upstream_req.header(name.as_str(), v);
        }
    }

    // Read request body; compress JSON bodies when enabled.
    let body_bytes = match axum::body::to_bytes(req.into_body(), 64 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                format!("failed reading body: {e}"),
            )
                .into_response()
        }
    };
    let bytes_in = body_bytes.len() as i64;

    // Pick a compression profile: the request's `model` field can override
    // the server default (small-context models get squeezed harder).
    let request_model: Option<String> = if !body_bytes.is_empty() && body_bytes[0] == b'{' {
        serde_json::from_slice::<serde_json::Value>(&body_bytes)
            .ok()
            .and_then(|v| v.get("model")?.as_str().map(str::to_string))
    } else {
        None
    };
    let profile = profiles::for_model(request_model.as_deref(), &state.profile);

    let final_body = if state.passthrough || body_bytes.is_empty() {
        body_bytes.to_vec()
    } else {
        maybe_compress_json_bytes_cached(&state, &body_bytes, &profile.config)
    };

    upstream_req = upstream_req.body(final_body);

    // Send upstream. Stream the response back untouched (SSE-safe).
    let upstream_resp = match upstream_req.send().await {
        Ok(r) => r,
        Err(e) => {
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                format!("upstream error: {e}"),
            )
                .into_response()
        }
    };

    let status = upstream_resp.status();

    let is_stream = upstream_resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("text/event-stream"))
        .unwrap_or(false);

    if is_stream {
        // SSE: stream through untouched. Stats count raw bytes only.
        let mut resp = Response::builder().status(status);
        let headers = resp.headers_mut().unwrap();
        for (name, value) in upstream_resp.headers() {
            if matches!(
                name.as_str(),
                "transfer-encoding" | "content-length" | "connection" | "content-encoding"
            ) {
                continue;
            }
            headers.insert(name.clone(), value.clone());
        }
        let stream = upstream_resp.bytes_stream();
        let body = Body::from_stream(stream);
        record_stats(&state, &path, bytes_in, 0);
        return match resp.body(body) {
            Ok(r) => r,
            Err(e) => (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("response build error: {e}"),
            )
                .into_response(),
        };
    }

    // Non-streaming: snapshot headers, then read full body and compress response JSON too.
    let upstream_headers: Vec<(axum::http::HeaderName, axum::http::HeaderValue)> = upstream_resp
        .headers()
        .iter()
        .filter(|(name, _)| {
            !matches!(
                name.as_str(),
                "transfer-encoding" | "content-length" | "connection" | "content-encoding"
            )
        })
        .map(|(n, v)| (n.clone(), v.clone()))
        .collect();

    let resp_bytes = match upstream_resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                format!("upstream body error: {e}"),
            )
                .into_response()
        }
    };
    let final_resp = if state.passthrough {
        resp_bytes.to_vec()
    } else {
        maybe_compress_json_bytes_cached(&state, &resp_bytes, &profile.config)
    };
    let bytes_out = final_resp.len() as i64;
    record_stats(&state, &path, bytes_in, bytes_out);

    let mut resp = Response::builder().status(status);
    {
        let headers = resp.headers_mut().unwrap();
        for (name, value) in upstream_headers {
            headers.insert(name, value);
        }
    }
    match resp.body(Body::from(final_resp)) {
        Ok(r) => r,
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("response build error: {e}"),
        )
            .into_response(),
    }
}

/// Try to parse bytes as JSON and compress string fields, with caching.
/// On ANY failure or non-JSON input, returns the original bytes unchanged (F5 safety).
/// Cache hit increments the hit counter and returns the stored result.
fn maybe_compress_json_bytes_cached(
    state: &ProxyState,
    bytes: &[u8],
    cfg: &CompressConfig,
) -> Vec<u8> {
    if bytes.len() < 2 {
        return bytes.to_vec();
    }
    let first = bytes[0];
    if first != b'{' && first != b'[' {
        return bytes.to_vec();
    }

    // Check cache first.
    if let Some(cache) = &state.cache {
        let hash = cache::hash_bytes(bytes);
        if let Ok(Some(cached)) = cache.lock().unwrap().get(&hash) {
            state.cache_hits.fetch_add(1, Ordering::Relaxed);
            return cached;
        }
    }

    // Cache miss: compress.
    let parsed: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => return bytes.to_vec(),
    };
    let (compressed, _saved) = compress::compress_json_value_with(parsed, cfg);
    match serde_json::to_vec(&compressed) {
        Ok(out) if out.len() < bytes.len() => {
            // Store in cache.
            if let Some(cache) = &state.cache {
                let hash = cache::hash_bytes(bytes);
                let _ = cache.lock().unwrap().put(&hash, bytes.len(), out.len(), &out);
            }
            out
        }
        _ => bytes.to_vec(),
    }
}

fn record_stats(state: &ProxyState, path: &str, bytes_in: i64, bytes_out: i64) {
    if let Ok(store) = state.stats.lock() {
        let _ = store.record(path, bytes_in, bytes_out);
    }
}

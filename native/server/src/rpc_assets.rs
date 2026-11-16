//! rpc_assets module — see task.
use crate::handlers::RpcResult;
use crate::AppState;
use serde_json::Value;
use std::sync::Arc;

pub async fn handle(state: Arc<AppState>, tag: &str, payload: Value) -> RpcResult {
    let _ = (state, payload);
    RpcResult::err(serde_json::json!({"_tag":"RpcClientError","message":format!("not implemented: {tag}")}))
}

/// Claim a subscription stream tag; returns true if this module handled it.
pub async fn handle_subscription(
    state: Arc<AppState>,
    out: crate::ws_rpc::OutTx,
    request_id: u64,
    tag: &str,
    payload: Value,
    window: Arc<crate::ws_rpc::Window>,
) -> bool {
    let _ = (state, out, request_id, tag, payload, window);
    false
}

/// GET /api/assets/<key> — serve a persisted attachment/asset blob.
/// Implemented alongside the assets.* RPCs in this module.
pub async fn serve_asset(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(key): axum::extract::Path<String>,
) -> axum::response::Response {
    let _ = (state, key);
    axum::http::Response::builder()
        .status(axum::http::StatusCode::NOT_FOUND)
        .body(axum::body::Body::empty())
        .unwrap()
}

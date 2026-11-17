//! `mcpApps.*` — MCP app bridge (wave-3).
//!
//! Contract tags (`packages/contracts/src/rpc.ts`): `mcpApps.callTool`,
//! `mcpApps.toolInfo`, `mcpApps.readResource`, `mcpApps.updateModelContext`.
//! Stub — replaced by the integrations parity agent.

use serde_json::{Value, json};
use std::sync::Arc;

use crate::AppState;
use crate::handlers::RpcResult;

pub async fn handle(_state: Arc<AppState>, _tag: &str, _payload: Value) -> RpcResult {
    RpcResult::err(json!({
        "_tag": "UnsupportedOperationError",
        "message": "mcpApps.* is not implemented on the Rust server yet",
    }))
}

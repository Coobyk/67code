use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::domain;
use crate::AppState;

const TICKET_TTL: Duration = Duration::from_secs(120);
/// Pairing credentials: short-lived bearer tokens accepted by the ticket mint.
const PAIRING_TTL_SECS: i64 = 600;
/// Displayed session expiry; dev sessions are not reaped on a timer.
const SESSION_TTL_SECS: i64 = 30 * 24 * 3600;

/// Full grant for the dev token — `AuthStandardClientScopes` from
/// `packages/contracts/src/auth.ts`.
const DEV_SCOPES: &[&str] = &[
    "orchestration:read",
    "orchestration:operate",
    "settings:write",
    "providers:manage",
    "environment:maintain",
    "preview:operate",
    "diagnostics:read",
    "terminal:read",
    "terminal:operate",
    "source-control:write",
    "filesystem:read",
    "filesystem:write",
    "relay:read",
    "access:read",
    "access:write",
    "relay:write",
];

/// `legacyScopes` — the subset `authScopeResponse` keeps in `scopes`.
fn legacy_scopes(scopes: &[String]) -> Vec<String> {
    const LEGACY: &[&str] = &[
        "orchestration:read",
        "orchestration:operate",
        "terminal:operate",
        "access:read",
        "access:write",
        "relay:read",
        "relay:write",
    ];
    scopes
        .iter()
        .filter(|s| LEGACY.contains(&s.as_str()))
        .cloned()
        .collect()
}

/// `mcpClientScopes(access)` — derive grant scopes from a pairing access level.
fn scopes_for_access(access: &str) -> Vec<String> {
    match access {
        "read-only" => vec!["orchestration:read"],
        "full-access" => vec![
            "orchestration:read",
            "orchestration:operate",
            "settings:write",
            "source-control:write",
        ],
        _ => vec!["orchestration:read", "orchestration:operate"],
    }
    .into_iter()
    .map(str::to_owned)
    .collect()
}

// ---------------------------------------------------------------------------
// Tickets
// ---------------------------------------------------------------------------

/// What a minted websocket ticket remembers about its bearer credential.
#[derive(Clone)]
pub struct TicketGrant {
    /// The bearer token presented at mint (dev token or pairing token).
    pub credential: String,
    pub label: Option<String>,
    pub scopes: Vec<String>,
}

pub struct TicketStore {
    /// ticket -> grant + issued-at. Single-use; consumed on the WS handshake.
    tickets: StdMutex<HashMap<String, (Instant, TicketGrant)>>,
}

impl TicketStore {
    pub fn new() -> Self {
        Self {
            tickets: StdMutex::new(HashMap::new()),
        }
    }

    pub fn mint(&self, grant: TicketGrant) -> String {
        let ticket = uuid::Uuid::new_v4().to_string();
        self.tickets
            .lock()
            .unwrap()
            .insert(ticket.clone(), (Instant::now(), grant));
        ticket
    }

    /// Consume a ticket. Valid only once and only within the TTL.
    pub fn take(&self, ticket: &str) -> Option<TicketGrant> {
        let mut map = self.tickets.lock().unwrap();
        map.retain(|_, (at, _)| at.elapsed() < TICKET_TTL);
        map.remove(ticket).map(|(_, grant)| grant)
    }
}

// ---------------------------------------------------------------------------
// Client sessions + pairing links (in-memory auth-access state)
// ---------------------------------------------------------------------------

/// One authorized client connection — emitted as `AuthClientSession` rows.
#[derive(Clone)]
pub struct ClientSession {
    pub session_id: String,
    pub credential: String,
    pub label: Option<String>,
    pub scopes: Vec<String>,
    pub issued_at: String,
    pub expires_at: String,
    pub last_connected_at: String,
}

/// A minted pairing credential — emitted as `AuthPairingLink` rows.
#[derive(Clone)]
pub struct PairingLink {
    pub id: String,
    pub token: String,
    pub label: Option<String>,
    pub scopes: Vec<String>,
    pub created_at: String,
    pub expires_at: String,
}

/// Auth-access change events broadcast to `subscribeAuthAccess` streams.
#[derive(Clone)]
pub enum AuthAccessChange {
    ClientUpserted(ClientSession),
    ClientRemoved { session_id: String },
    PairingLinkUpserted(PairingLink),
    PairingLinkRemoved { id: String },
}

/// Shared auth-access state on `AppState`.
pub struct AuthAccess {
    /// Live sessions keyed by session id (one per websocket connection).
    pub sessions: StdMutex<HashMap<String, ClientSession>>,
    /// session id -> close handle for that connection's socket.
    pub closers: StdMutex<HashMap<String, CancellationToken>>,
    /// Pairing links keyed by link id.
    pub pairing_links: StdMutex<HashMap<String, PairingLink>>,
    /// Change stream for subscribers.
    pub changes: broadcast::Sender<AuthAccessChange>,
}

impl AuthAccess {
    pub fn new() -> Self {
        let (changes, _rx) = broadcast::channel(256);
        Self {
            sessions: StdMutex::new(HashMap::new()),
            closers: StdMutex::new(HashMap::new()),
            pairing_links: StdMutex::new(HashMap::new()),
            changes,
        }
    }

    /// Register a session for a freshly upgraded websocket. Returns the id.
    pub fn register_session(&self, grant: &TicketGrant, close: CancellationToken) -> String {
        let now = domain::now_iso();
        let session = ClientSession {
            session_id: domain::new_id(),
            credential: grant.credential.clone(),
            label: grant.label.clone().or_else(|| Some("native client".into())),
            scopes: grant.scopes.clone(),
            issued_at: now.clone(),
            expires_at: domain::iso_in_seconds(SESSION_TTL_SECS),
            last_connected_at: now,
        };
        let id = session.session_id.clone();
        self.sessions
            .lock()
            .unwrap()
            .insert(id.clone(), session.clone());
        self.closers.lock().unwrap().insert(id.clone(), close);
        let _ = self.changes.send(AuthAccessChange::ClientUpserted(session));
        id
    }

    /// Remove a session (disconnect or revoke). Returns the removed record.
    pub fn remove_session(&self, session_id: &str) -> Option<ClientSession> {
        let removed = self.sessions.lock().unwrap().remove(session_id);
        if removed.is_some() {
            self.closers.lock().unwrap().remove(session_id);
            let _ = self
                .changes
                .send(AuthAccessChange::ClientRemoved {
                    session_id: session_id.to_string(),
                });
        }
        removed
    }

    /// Close the socket bound to a session without removing the record here
    /// (the connection task removes it on exit).
    pub fn close_session_socket(&self, session_id: &str) {
        if let Some(close) = self.closers.lock().unwrap().get(session_id) {
            close.cancel();
        }
    }

    /// Revoke a session: remove the record and drop its live socket.
    pub fn revoke_session(&self, session_id: &str) -> bool {
        let close = self.closers.lock().unwrap().get(session_id).cloned();
        if self.remove_session(session_id).is_some() {
            if let Some(close) = close {
                close.cancel();
            }
            true
        } else {
            false
        }
    }

    /// Removes a pairing link and broadcasts `pairingLinkRemoved`.
    pub fn revoke_pairing_link(&self, id: &str) -> bool {
        if self.pairing_links.lock().unwrap().remove(id).is_some() {
            let _ = self
                .changes
                .send(AuthAccessChange::PairingLinkRemoved { id: id.to_string() });
            true
        } else {
            false
        }
    }

    /// The credential (bearer token) a session was minted under, used to
    /// resolve "the caller's own session" for revoke-others.
    pub fn current_session_for_credential(&self, credential: &str) -> Option<String> {
        let sessions = self.sessions.lock().unwrap();
        sessions
            .values()
            .filter(|s| s.credential == credential)
            .max_by(|a, b| a.last_connected_at.cmp(&b.last_connected_at))
            .map(|s| s.session_id.clone())
    }

    pub fn pairing_token_valid(&self, token: &str) -> bool {
        let links = self.pairing_links.lock().unwrap();
        let now = domain::now_iso();
        links
            .values()
            .any(|l| l.token == token && l.expires_at > now)
    }

    /// `AuthClientSession` wire shape (`current` is relative to the
    /// subscribing connection's session id).
    pub fn session_wire(session: &ClientSession, current_session_id: &str) -> Value {
        let mut client = json!({ "deviceType": "unknown" });
        if let Some(label) = &session.label {
            client["label"] = json!(label);
        }
        json!({
            "sessionId": session.session_id,
            "subject": "dev",
            "scopes": legacy_scopes(&session.scopes),
            "permissions": session.scopes,
            "method": "bearer-access-token",
            "client": client,
            "issuedAt": session.issued_at,
            "expiresAt": session.expires_at,
            "lastConnectedAt": session.last_connected_at,
            "connected": true,
            "current": session.session_id == current_session_id,
        })
    }

    /// `AuthPairingLink` wire shape.
    pub fn pairing_link_wire(link: &PairingLink) -> Value {
        let mut obj = json!({
            "id": link.id,
            "subject": "dev",
            "scopes": legacy_scopes(&link.scopes),
            "permissions": link.scopes,
            "createdAt": link.created_at,
            "expiresAt": link.expires_at,
        });
        if let Some(label) = &link.label {
            obj["label"] = json!(label);
        }
        obj
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned)
}

/// Dev token match, or a live pairing token.
fn authorized(state: &AppState, credential: &str) -> bool {
    match &state.auth_token {
        Some(expected) if credential == expected.as_str() => true,
        None => true,
        _ => state.auth_access.pairing_token_valid(credential),
    }
}

/// `POST /api/auth/websocket-ticket/rpc`
/// Headers: `Authorization: Bearer <token>` → `{ "ticket": "<opaque>" }`.
pub async fn issue_ticket(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, StatusCode> {
    let Some(credential) = bearer(&headers) else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    if !authorized(&state, &credential) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    // Pairing tokens carry their own scopes and label.
    let (scopes, link_label) = {
        let links = state.auth_access.pairing_links.lock().unwrap();
        links
            .values()
            .find(|l| l.token == credential)
            .map(|l| (l.scopes.clone(), l.label.clone()))
            .unwrap_or_else(|| {
                (
                    DEV_SCOPES.iter().map(|s| s.to_string()).collect(),
                    None,
                )
            })
    };
    let label = body
        .and_then(|Json(b)| b.get("label").and_then(Value::as_str).map(str::to_owned))
        .or(link_label);
    let ticket = state.tickets.mint(TicketGrant {
        credential,
        label,
        scopes,
    });
    Ok(Json(json!({
        "ticket": ticket,
        "expiresAt": domain::iso_in_seconds(TICKET_TTL.as_secs() as i64),
    })))
}

/// `POST /api/auth/pairing-token` — mint a pairing credential.
/// Input `{label?, scopes?, access?}` → `AuthPairingCredentialResult` plus
/// `pairingUrl`/`token` conveniences for dev pairing.
pub async fn create_pairing_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, StatusCode> {
    let Some(credential) = bearer(&headers) else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    if !authorized(&state, &credential) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let body = body.map(|Json(b)| b).unwrap_or(json!({}));
    let label = body
        .get("label")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let access = body.get("access").and_then(Value::as_str);
    let scopes: Vec<String> = body
        .get("scopes")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .filter(|v: &Vec<String>| !v.is_empty())
        .or_else(|| access.map(scopes_for_access))
        .unwrap_or_else(|| DEV_SCOPES.iter().map(|s| s.to_string()).collect());

    let link = PairingLink {
        id: domain::new_id(),
        token: uuid::Uuid::new_v4().to_string(),
        label: label.clone(),
        scopes,
        created_at: domain::now_iso(),
        expires_at: domain::iso_in_seconds(PAIRING_TTL_SECS),
    };
    state
        .auth_access
        .pairing_links
        .lock()
        .unwrap()
        .insert(link.id.clone(), link.clone());
    let _ = state
        .auth_access
        .changes
        .send(AuthAccessChange::PairingLinkUpserted(link.clone()));

    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(|| state.listen_addr.to_string());
    let pairing_url = format!("http://{host}/pair#token={}", link.token);

    let mut result = json!({
        "id": link.id,
        "credential": link.token,
        "expiresAt": link.expires_at,
        "pairingUrl": pairing_url,
        "token": link.token,
    });
    if let Some(l) = &label {
        result["label"] = json!(l);
    }
    if let Some(a) = access {
        result["access"] = json!(a);
    }
    Ok(Json(result))
}

/// `POST /api/auth/clients/revoke` `{sessionId}` → `{revoked}`.
/// Removes the session and drops its live websocket.
pub async fn revoke_client(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, StatusCode> {
    let Some(credential) = bearer(&headers) else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    if !authorized(&state, &credential) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let session_id = body
        .and_then(|Json(b)| b.get("sessionId").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    if session_id.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let revoked = state.auth_access.revoke_session(&session_id);
    Ok(Json(json!({ "revoked": revoked })))
}

/// `POST /api/auth/pairing-links/revoke` `{id}` → `{revoked}`.
pub async fn revoke_pairing_link(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, StatusCode> {
    let Some(credential) = bearer(&headers) else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    if !authorized(&state, &credential) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let id = body
        .and_then(|Json(b)| b.get("id").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    if id.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(Json(json!({
        "revoked": state.auth_access.revoke_pairing_link(&id)
    })))
}

/// `POST /api/auth/clients/revoke-others` → `{revokedCount}`.
/// The caller's own session is the one minted under the same bearer token
/// (optionally overridden with `{sessionId}`/`{currentSessionId}` in the body).
pub async fn revoke_other_clients(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, StatusCode> {
    let Some(credential) = bearer(&headers) else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    if !authorized(&state, &credential) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let explicit = body.and_then(|Json(b)| {
        b.get("sessionId")
            .or_else(|| b.get("currentSessionId"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    let current = explicit
        .filter(|id| state.auth_access.sessions.lock().unwrap().contains_key(id))
        .or_else(|| state.auth_access.current_session_for_credential(&credential));

    let targets: Vec<String> = state
        .auth_access
        .sessions
        .lock()
        .unwrap()
        .keys()
        .filter(|id| Some(*id) != current.as_ref())
        .cloned()
        .collect();
    let mut revoked = 0usize;
    for id in targets {
        if state.auth_access.revoke_session(&id) {
            revoked += 1;
        }
    }
    Ok(Json(json!({ "revokedCount": revoked })))
}

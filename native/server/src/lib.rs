pub mod auth;
pub mod domain;
pub mod handlers;
pub mod provider;
pub mod store;
pub mod ws_rpc;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Mutex as StdMutex;

use axum::routing::{get, post};
use axum::Router;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::store::{CommittedEvent, StoreHandle};

/// Provider turns in flight, keyed by thread id. `thread.stop` cancels them.
pub type TurnRegistry = StdMutex<HashMap<String, Vec<CancellationToken>>>;

pub struct AppState {
    /// Dev auth token; when `None`, any bearer token is accepted.
    pub auth_token: Option<String>,
    pub tickets: auth::TicketStore,
    pub store: StoreHandle,
    /// Broadcast of committed domain events (sequences assigned at commit).
    pub events_tx: broadcast::Sender<CommittedEvent>,
    pub turns: TurnRegistry,
    /// Directory holding `statev2.sqlite` and provider state.
    pub home_dir: PathBuf,
    /// Directory containing dev replay fixtures (`apps/server/scripts/dev-replay`).
    pub replay_dir: PathBuf,
    /// Workspace root for the seeded "Local workspace" project.
    pub project_dir: PathBuf,
}

/// Everything the server needs; no env lookup inside `run`.
pub struct ServerConfig {
    /// Bind address; port 0 picks a free ephemeral port.
    pub bind_addr: SocketAddr,
    /// Dev auth token; `None` accepts any bearer token.
    pub auth_token: Option<String>,
    /// T3 home; the SQLite DB lives at `<home>/userdata/statev2.sqlite`.
    pub home_dir: PathBuf,
    /// Fixture dir containing `showcase_transcript.ndjson`; `None` discovers
    /// `apps/server/scripts/dev-replay` by walking cwd ancestors.
    pub replay_dir: Option<PathBuf>,
    /// Fixture DB copied to `<home>/userdata/statev2.sqlite` when missing.
    pub seed_db: Option<PathBuf>,
    /// Workspace root used for the seeded project.
    pub project_dir: PathBuf,
}

impl ServerConfig {
    /// Classic env-driven configuration (standalone `c67-server` binary).
    pub fn from_env() -> Self {
        let env = |names: &[&str]| {
            names.iter().find_map(|n| std::env::var(n).ok().filter(|v| !v.is_empty()))
        };
        let port: u16 = env(&["C67_PORT", "CODE67_PORT", "T3CODE_PORT"])
            .and_then(|v| v.parse().ok())
            .unwrap_or(13773);
        Self {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], port)),
            auth_token: env(&[
                "C67_DEV_AUTH_TOKEN",
                "CODE67_DEV_AUTH_TOKEN",
                "T3CODE_DEV_AUTH_TOKEN",
                "C67_AUTH_TOKEN",
            ]),
            home_dir: env(&["C67_HOME", "CODE67_HOME", "T3CODE_HOME"])
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(".t3")),
            replay_dir: env(&["C67_REPLAY_DIR"]).map(PathBuf::from),
            seed_db: env(&["C67_SEED_DB"]).map(PathBuf::from),
            project_dir: env(&["C67_PROJECT_DIR", "CODE67_PROJECT_DIR"])
                .map(PathBuf::from)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("/")),
        }
    }
}

fn discover_replay_dir() -> PathBuf {
    if let Ok(cwd) = std::env::current_dir() {
        let mut dir = cwd.as_path();
        loop {
            let candidate = dir.join("apps/server/scripts/dev-replay");
            if candidate.join("showcase_transcript.ndjson").exists() {
                return candidate;
            }
            match dir.parent() {
                Some(p) => dir = p,
                None => break,
            }
        }
    }
    PathBuf::from("apps/server/scripts/dev-replay")
}

/// Run the server until the process exits. If `ready` is given, the actual
/// bound address is sent once the listener is up (matters for port 0).
pub async fn run(
    config: ServerConfig,
    ready: Option<tokio::sync::oneshot::Sender<SocketAddr>>,
) -> anyhow::Result<()> {
    let data_dir = config.home_dir.join("userdata");
    std::fs::create_dir_all(&data_dir)?;
    let db_path = data_dir.join("statev2.sqlite");

    if !db_path.exists() {
        if let Some(seed) = &config.seed_db {
            if seed.exists() {
                tracing::info!(from = %seed.display(), "seeding statev2.sqlite from fixture");
                std::fs::copy(seed, &db_path)?;
            }
        }
    }

    let replay_dir = config.replay_dir.unwrap_or_else(discover_replay_dir);
    let store = StoreHandle::spawn(&db_path)?;
    let (events_tx, _rx) = broadcast::channel(2048);

    let state = std::sync::Arc::new(AppState {
        auth_token: config.auth_token,
        tickets: auth::TicketStore::new(),
        store,
        events_tx,
        turns: StdMutex::new(HashMap::new()),
        home_dir: config.home_dir,
        replay_dir,
        project_dir: config.project_dir,
    });

    // Guarantee at least one project so clients have somewhere to create threads.
    handlers::ensure_seed_project(&state).await?;

    let app = Router::new()
        .route("/api/health", get(|| async { "ok" }))
        .route("/api/auth/websocket-ticket/rpc", post(auth::issue_ticket))
        .route("/ws", get(ws_rpc::ws_upgrade))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, "c67-server listening");
    if let Some(ready) = ready {
        let _ = ready.send(addr);
    }
    axum::serve(listener, app).await?;
    Ok(())
}

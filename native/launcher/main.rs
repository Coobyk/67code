//! Root launcher: `cargo run --release` from the repo root.
//!
//! Starts c67-server in-process (ephemeral port, generated dev token,
//! C67_HOME=.t3) then runs the GPUI app on the main thread, wiring the
//! server URL + token directly — no C67_* env vars needed.

use std::net::SocketAddr;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "c67=info,c67_server=info".into()),
        )
        .init();

    let token = uuid::Uuid::new_v4().to_string();
    let project_dir = std::env::current_dir()?;
    let home_dir = PathBuf::from(".t3");

    let config = c67_server::ServerConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        auth_token: Some(token.clone()),
        home_dir,
        replay_dir: None,
        seed_db: None,
        project_dir: project_dir.clone(),
    };

    // Server on its own tokio runtime + thread; GPUI owns the main thread.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<SocketAddr>();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("c67-server runtime failed: {e}");
                return;
            }
        };
        if let Err(e) = rt.block_on(c67_server::run(config, Some(ready_tx))) {
            eprintln!("c67-server exited: {e}");
        }
    });

    // Wait for the bound address (ephemeral port resolves to a real one here).
    let addr = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt")
            .block_on(ready_rx)
    })
    .join()
    .map_err(|_| anyhow::anyhow!("ready thread panicked"))?
    .map_err(|_| anyhow::anyhow!("c67-server exited before binding"))?;

    let http_url = format!("http://{addr}");
    tracing::info!(%http_url, "server ready, starting app");

    // GPUI must run on the main thread; blocks until the window closes.
    c67_app::run(c67_app::AppConfig {
        endpoint: Some(c67_app::net::ServerEndpoint {
            http_url,
            token,
        }),
        provider: std::env::var("C67_PROVIDER").unwrap_or_else(|_| "devin".into()),
        model: std::env::var("C67_MODEL").unwrap_or_else(|_| "swe-2-high".into()),
    });
    Ok(())
}

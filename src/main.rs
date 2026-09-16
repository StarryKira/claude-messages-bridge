use claude_messages_bridge::{AppState, config::Config, router};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let config = Config::from_env().map_err(std::io::Error::other)?;
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(address=%listener.local_addr()?, cli=%config.cli.display(), "Anthropic Messages bridge listening");
    let state = AppState::try_new(config)?;
    let shutdown = state.shutdown.clone();
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async move {
            #[cfg(unix)]
            {
                let mut term =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("SIGTERM handler");
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
            shutdown.cancel();
        })
        .await?;
    Ok(())
}

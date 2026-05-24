use denia::{
    app::{AppState, build_router},
    config::AppConfig,
    state::SqliteStore,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = AppConfig::from_env()?;
    let store = SqliteStore::open(&config.database_path)?;
    store.migrate()?;

    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    let app = build_router(AppState::new(config, store));
    axum::serve(listener, app).await?;
    Ok(())
}

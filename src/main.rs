use denia::{
    app::{AppState, build_router},
    cgroup_launcher,
    config::AppConfig,
    socket_proxy,
    state::SqliteStore,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os();
    let argv0 = args.next();
    if argv0
        .as_ref()
        .and_then(|path| std::path::Path::new(path).file_name())
        .is_some_and(|name| name == "socket-proxy")
    {
        socket_proxy::run_from_args(args).await?;
        return Ok(());
    }
    if args
        .next()
        .as_deref()
        .is_some_and(|arg| arg == cgroup_launcher::MODE_ARG)
    {
        cgroup_launcher::run_from_args(args)?;
        return Ok(());
    }

    let config = AppConfig::from_env()?;
    let store = SqliteStore::open(&config.database_path)?;
    store.migrate()?;

    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    let app = build_router(AppState::new(config, store));
    axum::serve(listener, app).await?;
    Ok(())
}

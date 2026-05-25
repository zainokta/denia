use std::sync::Arc;

use denia::{
    app::{AppState, build_router},
    cgroup_launcher,
    config::AppConfig,
    scheduler::{Scheduler, run_until_shutdown},
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

    let orphans = store.fail_orphan_runs()?;
    if orphans > 0 {
        eprintln!("recovered {orphans} orphaned job run(s)");
    }

    let (scheduler, _enqueue_rx) = Scheduler::new(store.clone());
    let scheduler = Arc::new(scheduler);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let scheduler_task = tokio::spawn(run_until_shutdown(scheduler.clone(), shutdown_rx));

    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    let app = build_router(AppState::new(config, store));
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;

    let _ = shutdown_tx.send(());
    let _ = scheduler_task.await;
    Ok(())
}

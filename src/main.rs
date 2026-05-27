use std::net::SocketAddr;
use std::sync::Arc;

use denia::{
    app::{AppState, build_router},
    config::AppConfig,
    ingress::traefik_supervisor::{HostTraefikSpawner, TraefikSupervisor, acquire_and_prepare},
    oci::{registry::RegistryImagePuller, unpack::TarRootfsUnpacker},
    scheduler::{Scheduler, run_until_shutdown},
    socket_proxy,
    state::SqliteStore,
    workload_launcher,
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
    if argv0
        .as_ref()
        .and_then(|path| std::path::Path::new(path).file_name())
        .is_some_and(|name| name == "workload-launcher")
    {
        let code = workload_launcher::run_from_args(args)?;
        std::process::exit(code);
    }

    let config = AppConfig::from_env()?;
    let store = SqliteStore::open(&config.database_path)?;
    store.migrate()?;

    let orphans = store.fail_orphan_runs()?;
    if orphans > 0 {
        eprintln!("recovered {orphans} orphaned job run(s)");
    }

    let state = AppState::new(config.clone(), &store);
    let tls_in_use = state
        .services
        .list_services()
        .map_err(anyhow::Error::from)?
        .iter()
        .any(|s| s.tls_enabled);
    state.config.require_acme_email(tls_in_use)?;

    let (scheduler, _enqueue_rx) = Scheduler::new(store.clone());
    let scheduler = Arc::new(scheduler);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let scheduler_task = tokio::spawn(run_until_shutdown(scheduler.clone(), shutdown_rx));

    let (traefik_shutdown_tx, traefik_shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);
    let traefik_task = {
        let config = config.clone();
        tokio::spawn(async move {
            if let Err(e) = std::fs::create_dir_all(&config.traefik_dir) {
                eprintln!("traefik dir create failed: {e}");
                return;
            }
            let puller = RegistryImagePuller::new(config.traefik_dir.clone());
            let unpacker = TarRootfsUnpacker::new();
            match acquire_and_prepare(&config, &puller, &unpacker).await {
                Ok(binary) => {
                    let spawner = HostTraefikSpawner {
                        binary,
                        config_file: config.traefik_dir.join("traefik.yml"),
                        cwd: config.traefik_dir.clone(),
                        log_path: config.log_dir.join("traefik.log"),
                    };
                    let sup = TraefikSupervisor {
                        spawner,
                        max_restarts_for_test: None,
                    };
                    let outcome = sup.run(traefik_shutdown_rx).await;
                    eprintln!("traefik supervisor exited: {outcome:?}");
                }
                Err(e) => eprintln!("traefik acquire failed: {e}"),
            }
        })
    };

    // Wire the autoscaler: hand the bridge its activation hook, run boot
    // reconcile once, then spawn the periodic control loop until shutdown.
    let autoscale_interval = config.autoscale_interval_s;
    let autoscaler_task = if let Some((supervisor, controller)) = state.autoscaler_handle() {
        supervisor
            .set_activator(Arc::new(denia::autoscale::controller::SharedController(
                controller.clone(),
            )))
            .await;
        {
            let mut c = controller.lock().await;
            let _ = c.reconcile_boot_all().await;
        }
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(denia::autoscale::controller::run_until_shutdown(
            controller,
            std::time::Duration::from_secs(autoscale_interval),
            rx,
        ));
        Some((tx, handle))
    } else {
        None
    };

    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    let app = build_router(state);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;

    let _ = shutdown_tx.send(());
    let _ = scheduler_task.await;
    let _ = traefik_shutdown_tx.send(()).await;
    let _ = traefik_task.await;
    if let Some((tx, handle)) = autoscaler_task {
        let _ = tx.send(());
        let _ = handle.await;
    }
    Ok(())
}

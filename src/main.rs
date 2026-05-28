use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use denia::{
    app::{AppState, build_router},
    config::AppConfig,
    ingress::pingora::{
        AcmeDriver, ChallengeStore, IngressServerConfig, RENEWAL_WINDOW_DAYS, build_server,
        load_certs_from_disk, persist_cert, run_server, select_renewals,
    },
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

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,denia=debug,tower_http=info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(true)
        .with_writer(std::io::stderr)
        .init();

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

    // --- In-process ACME (instant-acme, HTTP-01) ---------------------------
    //
    // Build ONE shared `ChallengeStore` and clone it into both the axum
    // acme-challenge handler (`AppState.acme_challenges`) and the issuer so the
    // `:80` challenge proxy and the order driver observe the same token map
    // (Chunk B carry-forward). The driver is built only when an email is set;
    // `require_acme_email` above guarantees one exists if any service uses TLS.
    let challenges: ChallengeStore = state.acme_challenges.clone();
    let acme_driver = match &config.acme_email {
        Some(email) => {
            match AcmeDriver::new(
                &config.tls_dir,
                &config.acme_directory_url,
                email,
                challenges.clone(),
            )
            .await
            {
                Ok(driver) => Some(Arc::new(driver)),
                Err(e) => {
                    eprintln!("acme driver init failed (cert issuance disabled): {e}");
                    None
                }
            }
        }
        None => None,
    };

    // Boot-load any persisted certs into the cert store BEFORE `:443` accepts so
    // a restart never re-orders every cert (LE rate limits) and never serves a
    // handshake with an empty store (Chunk B carry-forward).
    state
        .ingress
        .swap_certs(load_certs_from_disk(&config.tls_dir));

    // --- Pingora ingress server (dedicated thread, Denia-owned shutdown) ----
    //
    // Spawn on a dedicated `std::thread` (Spike 0.1: Pingora builds its own tokio
    // runtimes). Shutdown is modeled on the old Traefik mpsc pattern: a `watch`
    // channel flipped from the same graceful-shutdown point. A bind failure is
    // ISOLATED — it logs a clear `:80`/`:443`-in-use message and the control
    // plane keeps serving `bind_addr` (axum), never aborting the process (A6).
    let (pingora_shutdown_tx, pingora_shutdown_rx) = tokio::sync::watch::channel(false);
    let pingora_cfg =
        IngressServerConfig::from_ports(config.http_port, config.https_port, config.bind_addr);
    let pingora_thread = {
        let ingress = state.ingress.clone();
        std::thread::Builder::new()
            .name("denia-ingress".to_string())
            .spawn(move || match build_server(ingress, &pingora_cfg) {
                Ok(server) => run_server(server, pingora_shutdown_rx),
                Err(e) => eprintln!(
                    "ingress proxy failed to start ({e}); :80/:443 may be in use — Denia owns \
                     these ports. The control plane keeps serving the management API."
                ),
            })
            .ok()
    };

    // Issue certs for already-verified TLS hostnames, then scan for renewals on
    // an interval. Both run on Denia's runtime (issuance is async/out-of-band;
    // selection is the sync TlsAccept callback). Secrets discipline: never log
    // key authorizations, private keys, or the ACME account key.
    let acme_task = acme_driver.as_ref().map(|driver| {
        let driver = driver.clone();
        let ingress = state.ingress.clone();
        let tls_dir = config.tls_dir.clone();
        let services = state.services.clone();
        let domains = state.domains.clone();
        let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            // Initial issuance pass for verified TLS hostnames lacking a cert.
            issue_missing_certs(&driver, &ingress, &tls_dir, &services, &domains).await;
            let mut ticker = tokio::time::interval(Duration::from_secs(12 * 60 * 60));
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    _ = ticker.tick() => {
                        let due = select_renewals(&ingress.certs(), RENEWAL_WINDOW_DAYS);
                        for domain in due {
                            reissue(&driver, &ingress, &tls_dir, &domain).await;
                        }
                        issue_missing_certs(&driver, &ingress, &tls_dir, &services, &domains).await;
                    }
                }
            }
        });
        (tx, handle)
    });

    // Wire the autoscaler: hand the ingress its activation hook, run boot
    // reconcile once, then spawn the periodic control loop until shutdown.
    // OCI layer cache garbage collector (ADR-022). Cancel-safe loop, mirrors
    // the scheduler/ACME `run_until_shutdown` pattern. Failure to spawn must
    // not kill the control plane — the cache continues to fill, just without
    // automated cleanup, until `POST /v1/oci/cache/gc` is invoked manually.
    let oci_gc_task = state.oci_cache_gc.clone().map(|gc| {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let interval = std::time::Duration::from_secs(config.oci_gc_interval_secs);
        let handle = tokio::spawn(denia::oci::cache::gc_run_until_shutdown(gc, interval, rx));
        (tx, handle)
    });

    let autoscale_interval = config.autoscale_interval_s;
    let autoscaler_task = if let Some((ingress, controller)) = state.autoscaler_handle() {
        ingress
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
    // Tell the Pingora server to stop and join its thread (mirrors the old
    // traefik_shutdown send point).
    let _ = pingora_shutdown_tx.send(true);
    if let Some(thread) = pingora_thread {
        let _ = thread.join();
    }
    if let Some((tx, handle)) = acme_task {
        let _ = tx.send(());
        let _ = handle.await;
    }
    if let Some((tx, handle)) = autoscaler_task {
        let _ = tx.send(());
        let _ = handle.await;
    }
    if let Some((tx, handle)) = oci_gc_task {
        let _ = tx.send(());
        let _ = handle.await;
    }
    Ok(())
}

/// Issue certs for every verified hostname of a TLS-enabled service that does
/// not yet have one in the cert store. Persists atomically and swaps into the
/// live store. Never logs secret material.
async fn issue_missing_certs(
    driver: &AcmeDriver,
    ingress: &denia::ingress::pingora::IngressState,
    tls_dir: &std::path::Path,
    services: &denia::repo::sqlite::SqliteServiceRepo,
    domains: &denia::repo::sqlite::SqliteDomainRepo,
) {
    let Ok(all) = services.list_services() else {
        return;
    };
    for svc in all.into_iter().filter(|s| s.tls_enabled) {
        let Ok(hostnames) = domains.list_verified_hostnames(svc.id) else {
            continue;
        };
        for hostname in hostnames {
            if ingress.certs().get(&hostname).is_some() {
                continue;
            }
            reissue(driver, ingress, tls_dir, &hostname).await;
        }
    }
}

/// Drive one ACME order for `domain`, persist the result, and hot-swap it into
/// the live cert store. Errors are logged without leaking secret material.
async fn reissue(
    driver: &AcmeDriver,
    ingress: &denia::ingress::pingora::IngressState,
    tls_dir: &std::path::Path,
    domain: &str,
) {
    match driver.issue(domain).await {
        Ok(issued) => {
            if let Err(e) = persist_cert(tls_dir, domain, &issued) {
                eprintln!("failed to persist issued cert for {domain}: {e}");
                return;
            }
            // Reload the whole store from disk so the swap stays single-writer
            // and observes all persisted certs (A8).
            ingress.swap_certs(load_certs_from_disk(tls_dir));
        }
        Err(e) => eprintln!("acme issuance failed for {domain}: {e}"),
    }
}

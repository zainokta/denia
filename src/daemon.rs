//! Daemon entry point extracted from src/main.rs so the CLI dispatcher can
//! branch into it without re-implementing the boot sequence.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::{
    app::{AppState, build_router},
    config::AppConfig,
    deploy::coordinator::DeploymentCoordinator,
    domain::RuntimeInstanceId,
    ingress::pingora::{
        AcmeDriver, ChallengeStore, IngressServerConfig, RENEWAL_WINDOW_DAYS, build_server,
        load_certs_from_disk, persist_cert, run_server, select_renewals,
    },
    runtime::Runtime,
    scheduler::{Scheduler, run_until_shutdown},
    state::SqliteStore,
};

/// Run the full denia daemon (control plane + ingress + scheduler + ACME +
/// autoscaler + OCI GC). Called when the binary is invoked with no
/// subcommand.
pub async fn run() -> anyhow::Result<()> {
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

    // Each daemon process lifetime is a "session": wipe the previous session's
    // on-disk workload + deployment logs so every start begins with a clean log
    // tree. Done at startup (not shutdown) so unclean exits (SIGKILL/crash) are
    // also covered. Runs before orphan-deployment recovery so the synthetic
    // RESTART markers below land in the freshly emptied tree.
    match crate::observability::logs::clean_session_logs(&config.log_dir) {
        Ok(n) if n > 0 => tracing::info!(removed = n, "cleaned previous session logs"),
        Ok(_) => {}
        Err(error) => tracing::warn!(?error, "session log cleanup failed; continuing boot"),
    }

    let orphans = store.fail_orphan_runs()?;
    if orphans > 0 {
        eprintln!("recovered {orphans} orphaned job run(s)");
    }

    let orphan_deployments = store.fail_orphan_deployments()?;
    for id in &orphan_deployments {
        let path = crate::deploy::log::deployment_log_path(&config.log_dir, *id);
        if let Ok(writer) = crate::deploy::log::DeploymentLogWriter::create(&config.log_dir, *id) {
            let _ = writer.write("RESTART", "control plane restarted; deployment aborted");
        }
        tracing::warn!(deployment_id = %id, path = %path.display(), "orphan deployment marked Failed");
    }

    // Self-relocate daemon into <cgroup_root>/.daemon/ so subsequent workload
    // cgroup migrations are sibling-to-sibling under <cgroup_root> (kernel
    // skips the cross-subtree migration EINVAL we hit when started from
    // user/system slices). Best-effort; failure is logged but non-fatal.
    if let Err(error) = relocate_daemon_cgroup(&config.cgroup_root) {
        tracing::warn!(
            ?error,
            "daemon cgroup self-relocation failed (workload deploys may fail with EINVAL on cgroup.procs)"
        );
    }

    let state = AppState::new(config.clone(), &store);
    let tls_in_use = state
        .services
        .list_services()
        .map_err(anyhow::Error::from)?
        .iter()
        .any(|s| s.tls_enabled);
    state.config.require_acme_email(tls_in_use)?;

    // Reap workloads left behind by a previous unclean session (SIGKILL, crash,
    // power loss). `list_running` is empty on a fresh process, so neither the
    // autoscaler nor the plain-autostart below can see these survivors — a
    // filesystem + cgroup sweep is the only thing that reaps them. Runs BEFORE
    // any launcher so both start from a clean tree.
    match state.runtime.sweep_orphans().await {
        Ok(swept) if swept > 0 => {
            tracing::warn!(swept, "removed orphaned workloads from a previous session")
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(?error, "orphan sweep failed; continuing boot"),
    }

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
    let pingora_cfg = IngressServerConfig::from_ports(
        config.http_port,
        config.https_port,
        config.bind_addr,
        config.control_domain.clone(),
        config.control_tls,
    );
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
        let control_domain = config.control_domain.clone();
        let control_tls = config.control_tls;
        let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            // Initial issuance pass for verified TLS hostnames lacking a cert.
            issue_missing_certs(&driver, &ingress, &tls_dir, &services, &domains).await;
            if let Some(cd) = control_domain_to_issue(control_domain.as_deref(), control_tls) {
                let store = ingress.certs();
                if store.get(cd).is_none() {
                    reissue(&driver, &ingress, &tls_dir, cd).await;
                }
            }
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
                        if let Some(cd) = control_domain_to_issue(control_domain.as_deref(), control_tls) {
                            let store = ingress.certs();
                            if store.get(cd).is_none() {
                                reissue(&driver, &ingress, &tls_dir, cd).await;
                            }
                        }
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
        let handle = tokio::spawn(crate::oci::cache::gc_run_until_shutdown(gc, interval, rx));
        (tx, handle)
    });

    // Hosted OCI registry garbage collector (ADR-031). Same cancel-safe loop
    // as the layer cache GC. Conservative: never deletes blobs referenced by a
    // manifest or younger than the grace period, never touches active uploads.
    let registry_gc_task = {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let interval = std::time::Duration::from_secs(config.registry_gc_interval_secs);
        let handle = tokio::spawn(crate::registry::gc::gc_run_until_shutdown(
            state.registry_gc.clone(),
            interval,
            rx,
        ));
        (tx, handle)
    };

    let autoscale_interval = config.autoscale_interval_s;
    let autoscaler_task = if let Some((ingress, controller)) = state.autoscaler_handle() {
        ingress
            .set_activator(Arc::new(crate::autoscale::controller::SharedController(
                controller.clone(),
            )))
            .await;
        {
            let mut c = controller.lock().await;
            let _ = c.reconcile_boot_all().await;
        }
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(crate::autoscale::controller::run_until_shutdown(
            controller,
            std::time::Duration::from_secs(autoscale_interval),
            rx,
        ));
        Some((tx, handle))
    } else {
        None
    };

    // Autostart plain (non-autoscaled) services whose promoted deployment is
    // still set = "should be running". Autoscaled services were already brought
    // back by the autoscaler's `reconcile_boot_all` above; routing them through
    // the autoscaler would break the coordinator's single-replica convention.
    // (Runtime deploys of autoscaled services hand off to the controller via
    // `Controller::reconcile_service` on the deploy path — see ADR-028.)
    autostart_plain_promoted(&state).await;

    // Rebuild the ingress route table from persisted state so any service with a
    // verified domain is reachable on boot WITHOUT a redeploy. The autoscaler's
    // `reconcile_boot_all` and `autostart_plain_promoted` above repopulate the
    // replica pools, but the route snapshot starts empty every boot — autoscaled
    // services in particular never had their Host -> service_id route rewritten,
    // so they 404'd (and scale-to-zero services could never be woken) until a
    // redeploy. Runs after both launch paths so pools are populated before the
    // routes go live.
    if let Err(error) = crate::deploy::rebuild_routes_on_boot(&state) {
        tracing::warn!(
            ?error,
            "boot route table rebuild failed; domains may be unreachable until redeploy"
        );
    }

    // Clone a runtime handle before `build_router` consumes `state`, so the
    // shutdown path below can stop every running workload.
    let shutdown_runtime = state.runtime.clone();

    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    let app = build_router(state);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
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
    {
        let (tx, handle) = registry_gc_task;
        let _ = tx.send(());
        let _ = handle.await;
    }

    // Stop every running workload so nothing is orphaned across a daemon stop.
    // Runs AFTER the autoscaler task is joined (above) so no tick can launch a
    // replica mid-teardown. The DB is deliberately NOT mutated — promoted +
    // Healthy must survive so the next boot autostarts them. Bounded by a
    // timeout under systemd's TimeoutStopSec; stops run concurrently so N
    // replicas don't serialize N × the per-replica SIGTERM grace.
    stop_all_workloads(shutdown_runtime).await;
    Ok(())
}

/// Resolve when the daemon receives SIGINT (Ctrl+C) or SIGTERM (`systemctl
/// stop`). `tokio::signal::ctrl_c` alone only catches SIGINT, which would leave
/// the entire graceful-shutdown path dead under systemd.
async fn shutdown_signal() {
    let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(term) => term,
        Err(error) => {
            tracing::warn!(?error, "failed to install SIGTERM handler; SIGINT only");
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

/// Stop all in-memory-tracked workloads concurrently, bounded by a 25s timeout
/// (under the systemd `TimeoutStopSec=30s` ceiling). Best-effort: a single
/// replica failing to stop is logged, never propagated.
async fn stop_all_workloads(runtime: std::sync::Arc<dyn Runtime>) {
    let running = runtime.list_running().await.unwrap_or_default();
    if running.is_empty() {
        return;
    }
    tracing::info!(
        count = running.len(),
        "stopping running workloads on shutdown"
    );
    let stop_all = async {
        let mut set = tokio::task::JoinSet::new();
        for status in running {
            let runtime = runtime.clone();
            set.spawn(async move {
                let instance = RuntimeInstanceId {
                    service_id: status.service_id,
                    service_name: status.service_name.clone(),
                    replica_index: status.replica_index,
                };
                if let Err(error) = runtime.stop(&instance).await {
                    tracing::warn!(
                        service_id = %status.service_id,
                        replica = status.replica_index,
                        ?error,
                        "failed to stop workload during shutdown"
                    );
                }
            });
        }
        while set.join_next().await.is_some() {}
    };
    if tokio::time::timeout(Duration::from_secs(25), stop_all)
        .await
        .is_err()
    {
        tracing::warn!(
            "workload shutdown timed out; survivors will be reaped by the next boot's orphan sweep"
        );
    }
}

/// Restart plain (non-autoscaled) services whose promoted deployment is still
/// set. Best-effort per service: one failure (missing artifact, runtime error)
/// is logged and the rest continue.
async fn autostart_plain_promoted(state: &AppState) {
    let services = match state.services.list_services() {
        Ok(services) => services,
        Err(error) => {
            tracing::warn!(?error, "boot autostart: failed to list services");
            return;
        }
    };
    let coordinator = DeploymentCoordinator::new_with_shared_routing(
        state.deployment_repos(),
        state.runtime.clone(),
        state.health.clone(),
        state.ingress.clone(),
        state.routes.clone(),
    );
    for service in services.into_iter().filter(|s| s.autoscale.is_none()) {
        let promoted = match state.deployments.promoted_deployment(service.id) {
            Ok(promoted) => promoted,
            Err(error) => {
                tracing::warn!(service_id = %service.id, ?error, "boot autostart: promoted lookup failed");
                continue;
            }
        };
        let Some(deployment_id) = promoted else {
            continue;
        };
        let log = match crate::deploy::log::DeploymentLogWriter::create(
            &state.config.log_dir,
            deployment_id,
        ) {
            Ok(writer) => writer,
            Err(error) => {
                tracing::warn!(service_id = %service.id, ?error, "boot autostart: cannot open deployment log");
                continue;
            }
        };
        let _ = log.write("AUTOSTART", "restarting promoted deployment on boot");
        match coordinator.restart_promoted(&service, &log).await {
            Ok(()) => tracing::info!(
                service_id = %service.id,
                deployment_id = %deployment_id,
                "autostarted promoted deployment"
            ),
            Err(error) => {
                let _ = log.write("ERROR", &format!("autostart failed: {error:?}"));
                tracing::warn!(service_id = %service.id, ?error, "boot autostart failed; skipping");
            }
        }
    }
}

/// Move the daemon (current process + all sibling threads) into a dedicated
/// `<cgroup_root>/.daemon/` cgroup. Establishing the daemon under the same
/// cgroup root as workloads makes later `cgroup.procs` migrations
/// sibling-to-sibling — sidestepping the cross-subtree migration EINVAL that
/// triggers when the daemon was started from a delegated systemd scope
/// (e.g. `/system.slice/denia-dev.scope` or `/user.slice/...`).
fn relocate_daemon_cgroup(cgroup_root: &std::path::Path) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(cgroup_root)?;
    // Enable controllers on cgroup_root so children (.daemon and per-workload
    // leaves) inherit them. Read available controllers first to avoid writing
    // unsupported ones.
    let controllers_path = cgroup_root.join("cgroup.controllers");
    let subtree_path = cgroup_root.join("cgroup.subtree_control");
    if controllers_path.exists() && subtree_path.exists() {
        let available = std::fs::read_to_string(&controllers_path)?;
        let wanted: Vec<String> = ["cpu", "memory", "pids", "io"]
            .into_iter()
            .filter(|c| available.split_whitespace().any(|a| a == *c))
            .map(|c| format!("+{c}"))
            .collect();
        if !wanted.is_empty() {
            // Ignore failure: subtree_control may already be set with the same
            // values, in which case write returns EBUSY/0 depending on kernel.
            let _ = std::fs::write(&subtree_path, format!("{}\n", wanted.join(" ")));
        }
    }
    let daemon_cg = cgroup_root.join(".daemon");
    std::fs::create_dir_all(&daemon_cg)?;
    let procs = daemon_cg.join("cgroup.procs");
    let pid = std::process::id();
    // Move every thread of the current process into the daemon cgroup. In
    // cgroup v2, writing the TGID to cgroup.procs migrates all threads at
    // once.
    let mut f = std::fs::OpenOptions::new().write(true).open(&procs)?;
    // Single write — writeln! can split digits and newline, kernel parses
    // the empty trailing newline as a second write and returns EINVAL even
    // though the migration already succeeded.
    f.write_all(format!("{pid}\n").as_bytes())?;
    Ok(())
}

/// The control domain to ACME-issue, if TLS is enabled for it. Renewal is
/// automatic once the cert is in the store (`select_renewals` covers any SNI);
/// only the initial issuance needs this branch (the control domain has no
/// service row, so `issue_missing_certs` does not cover it).
fn control_domain_to_issue(control_domain: Option<&str>, control_tls: bool) -> Option<&str> {
    if control_tls { control_domain } else { None }
}

/// Issue certs for every verified hostname of a TLS-enabled service that does
/// not yet have one in the cert store. Persists atomically and swaps into the
/// live store. Never logs secret material.
async fn issue_missing_certs(
    driver: &AcmeDriver,
    ingress: &crate::ingress::pingora::IngressState,
    tls_dir: &std::path::Path,
    services: &crate::repo::sqlite::SqliteServiceRepo,
    domains: &crate::repo::sqlite::SqliteDomainRepo,
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
    ingress: &crate::ingress::pingora::IngressState,
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

#[cfg(test)]
mod tests {
    use super::control_domain_to_issue;

    #[test]
    fn control_domain_issued_only_when_tls_enabled() {
        assert_eq!(control_domain_to_issue(Some("denia.example.com"), true), Some("denia.example.com"));
        assert_eq!(control_domain_to_issue(Some("denia.example.com"), false), None);
        assert_eq!(control_domain_to_issue(None, true), None);
    }
}

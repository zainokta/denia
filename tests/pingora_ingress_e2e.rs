//! Live end-to-end test of the in-process Pingora ingress (ADR-020).
//!
//! Spins up the real Pingora `Server` (via `build_server`/`run_server`) on
//! ephemeral loopback ports, registers a route whose upstream is a fake HTTP
//! server listening on a Unix socket, and drives real TCP requests through the
//! proxy to assert:
//!   - a known `Host` is proxied to the workload UDS (200 + body), and
//!   - an unknown `Host` gets a 404 from the proxy itself.
//!
//! Marked `#[ignore]` (opt-in): it spawns a real proxy with its own tokio
//! runtimes on a dedicated thread and binds real (high, unprivileged) ports, so
//! it is kept out of the default `cargo test` run. It needs NO root — run with:
//!   `cargo test --test pingora_ingress_e2e -- --ignored`

use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use denia::ingress::pingora::{
    IngressServerConfig, IngressState, RouteSpec, RouteTable, build_server, run_server,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UnixListener};
use uuid::Uuid;

/// Grab a free loopback TCP port by binding `:0` and immediately dropping it.
/// Racy in theory, fine for a single-process test.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral")
        .local_addr()
        .expect("local addr")
        .port()
}

/// Minimal HTTP/1.1 server on a Unix socket that always replies 200 with a
/// fixed body. Models a Denia workload's host UDS endpoint.
async fn spawn_fake_uds_workload(sock_path: PathBuf) {
    let listener = UnixListener::bind(&sock_path).expect("bind uds workload");
    tokio::spawn(async move {
        loop {
            let Ok((mut conn, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                // Read (and ignore) the request head; one read is enough for a
                // tiny GET with no body.
                let _ = conn.read(&mut buf).await;
                let body = b"hello-from-uds";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = conn.write_all(resp.as_bytes()).await;
                let _ = conn.write_all(body).await;
                let _ = conn.flush().await;
            });
        }
    });
}

/// Send a raw HTTP/1.1 GET with the given Host through the proxy and return the
/// full response text. Times out so a misroute can't hang the suite.
async fn http_get(addr: SocketAddr, host: &str) -> String {
    let fut = async {
        let mut stream = TcpStream::connect(addr).await.expect("connect proxy");
        let req = format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
        stream.write_all(req.as_bytes()).await.expect("write req");
        let mut resp = Vec::new();
        stream.read_to_end(&mut resp).await.expect("read resp");
        String::from_utf8_lossy(&resp).into_owned()
    };
    tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .expect("request timed out")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "spawns a live Pingora server on real ports; run with --ignored"]
async fn proxies_known_host_to_uds_and_404s_unknown() {
    // --- fake workload on a UDS ---
    let tmp = std::env::temp_dir().join(format!("denia-e2e-{}.sock", Uuid::now_v7()));
    let _ = std::fs::remove_file(&tmp);
    spawn_fake_uds_workload(tmp.clone()).await;

    // --- ingress state: one healthy replica + a route for test.local ---
    let state = Arc::new(IngressState::default());
    let service_key = "websvc"; // pool key == RouteSpec.service_id (no activator needed)
    let replica = Uuid::now_v7();
    state.add_replica(service_key, replica, tmp.clone()).await;
    state.set_replica_healthy(service_key, replica, true).await;
    let mut table = RouteTable::default();
    table
        .try_upsert(RouteSpec {
            route_key: service_key.to_string(),
            service_name: "web".to_string(),
            service_id: service_key.to_string(),
            domains: vec!["test.local".to_string()],
            tls: false,
        })
        .expect("route upserts");
    state.swap_routes(table);

    // --- build + run the real Pingora server on ephemeral ports ---
    let http_port = free_port();
    let https_port = free_port();
    let control_port = free_port();
    let cfg = IngressServerConfig {
        http_addr: SocketAddr::from(([127, 0, 0, 1], http_port)),
        https_addr: SocketAddr::from(([127, 0, 0, 1], https_port)),
        control_backend: SocketAddr::from(([127, 0, 0, 1], control_port)),
    };
    let server = build_server(state, &cfg).expect("server builds");

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = std::thread::spawn(move || run_server(server, shutdown_rx));

    // --- wait for the :80 listener to come up ---
    let http_addr = cfg.http_addr;
    let mut up = false;
    for _ in 0..50 {
        if TcpStream::connect(http_addr).await.is_ok() {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(up, "proxy :80 never bound on {http_addr}");

    // --- known host → proxied to the UDS workload ---
    let ok = http_get(http_addr, "test.local").await;
    assert!(ok.contains("200 OK"), "expected 200, got:\n{ok}");
    assert!(
        ok.contains("hello-from-uds"),
        "expected UDS body, got:\n{ok}"
    );

    // --- unknown host → 404 from the proxy ---
    let nf = http_get(http_addr, "nope.local").await;
    assert!(nf.contains(" 404 "), "expected 404, got:\n{nf}");

    // --- shutdown + join ---
    let _ = shutdown_tx.send(true);
    let _ = tokio::task::spawn_blocking(move || handle.join()).await;
    let _ = std::fs::remove_file(&tmp);
}

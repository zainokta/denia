# Pingora Ingress De-risking Spikes — 2026-05-28

Three throwaway spikes gating the Traefik-process -> in-process Pingora proxy
migration for Denia (Rust / axum / tokio). All work done in `/tmp/pingora-spike`
(outside the repo). Findings below are backed by real build + run output, not
guesses.

## Environment

- Host: CachyOS, Linux 7.x, zsh.
- Toolchain present and sufficient: `cmake`, `perl`, `cc`, `gcc`, `clang` all on PATH.
- `rustc 1.95.0` (Arch), edition 2024.

## Pinned versions (from Cargo.lock)

| Crate | Version |
|-------|---------|
| `pingora` | **0.8.0** |
| `pingora-proxy` | **0.8.0** |
| `pingora-boringssl` | 0.8.0 |
| `boring` / `boring-sys` | 4.22.0 |
| `tokio` | 1.52.3 |

`Cargo.toml` features used:
```toml
pingora = { version = "0.8.0", features = ["proxy", "boringssl"] }
pingora-proxy = { version = "0.8.0", features = ["boringssl"] }
boring = "4.22.0"   # only for cert minting helpers (asn1/bn/rsa) in spike 0.3
```

## Build / system-dep result (the boringssl gate)

**No blocker.** The default TLS backend is boringssl. With cmake/perl/clang
present, `cargo build` with `features=["proxy","boringssl"]` compiled cleanly
(`boring-sys 4.22.0` built its vendored BoringSSL via cmake on the first build).

Notable: `pingora` ships **no TLS backend in its default feature set**. The
`proxy` feature alone has zero SSL deps (fast build, no C toolchain needed). TLS
is opt-in via exactly one of `boringssl`, `openssl`, `rustls`, or `s2n`. Denia
must explicitly select `boringssl` (recommended — see spike 0.3) at the manifest.

Without a TLS feature, `pingora_core::tls::ext::*` (`ssl_use_certificate`,
`suspend_when_need_ssl_cert`, etc.) are `panic!("Not implemented")` stubs — so
the per-SNI cert callback is *only* available with a real backend.

---

## Spike 0.1 — Signal handling — VERDICT: GREEN (with mandatory caveats)

**Question:** can a Pingora `Server` run WITHOUT installing its own
SIGTERM/SIGINT/SIGQUIT handlers, leaving signal ownership to Denia?

**Answer: YES.** Pingora's signal handling is fully injectable. `Server::run`
takes a `RunArgs` whose `shutdown_signal: Box<dyn ShutdownSignalWatch>` decides
*how* shutdown is detected. The default `UnixShutdownSignalWatch` is the only
thing that calls `tokio::signal::unix::signal(...)`. Provide a custom
`ShutdownSignalWatch` and **Pingora installs no OS signal handler at all.**

Key source (`pingora-core-0.8.0/src/server/mod.rs`):
```rust
pub trait ShutdownSignalWatch {
    async fn recv(&self) -> ShutdownSignal;
}
pub struct RunArgs {
    #[cfg(unix)]
    pub shutdown_signal: Box<dyn ShutdownSignalWatch>, // default: UnixShutdownSignalWatch
}
```

Working pattern (proven at runtime):
```rust
struct AppDrivenShutdown { rx: watch::Receiver<bool> }
#[async_trait]
impl ShutdownSignalWatch for AppDrivenShutdown {
    async fn recv(&self) -> ShutdownSignal {
        let mut rx = self.rx.clone();
        while !*rx.borrow() { if rx.changed().await.is_err() { break; } }
        ShutdownSignal::FastShutdown   // or GracefulTerminate
    }
}

// On a DEDICATED std::thread (NOT Denia's runtime):
let mut server = Server::new(None).unwrap();
server.bootstrap();
let mut proxy = http_proxy_service(&server.configuration, MyProxy);
proxy.add_tcp("127.0.0.1:18080");
server.add_service(proxy);
server.run(RunArgs { shutdown_signal: Box::new(AppDrivenShutdown { rx }) });
```

**Runtime evidence** (spike01_signal, process exit code 0):
```
[main] connected to pingora listener on :18080 OK
[main] raising SIGTERM at the process; Denia's handler must catch it...
[main] Denia's SIGTERM handler fired (Pingora did NOT swallow it)
[main] Denia now asks Pingora to stop via channel...
[pingora-thread] server.run() returned cleanly (no process::exit)
[main] pingora thread joined; Denia process still in control. EXIT 0
```
The test installs Denia's own `tokio::signal` SIGTERM handler, raises SIGTERM at
the PID, and confirms **Denia's** handler fires — Pingora never registered one.

**MANDATORY caveats (why GREEN, not unconditional):**
1. **Use `Server::run(RunArgs{..})`, NEVER `run_forever()`.** `run_forever()`
   calls `std::process::exit(0)` after the server loop and would kill the whole
   Denia process. `run()` returns normally. (`server/mod.rs:637` vs `:653`.)
2. **`Server` must run on its own dedicated `std::thread`.** `run()` builds its
   *own* tokio runtimes internally and `block_on`s a main loop; it cannot be
   awaited inside Denia's existing runtime. Spawn it via `std::thread::spawn`.
3. **`run()` has two `process::exit(1)` paths** — only on fatal startup errors:
   topological-sort failure of service deps, and a bootstrap-service failure
   (`server/bootstrap_services.rs:143/158`). These are startup-only, not signal
   paths, but Denia should treat a Pingora thread that exits the process as a
   hard fault. With a single proxy service and no upgrade/daemon config, these
   are not hit.
4. Do **not** enable `conf.daemon` (it forks) and do not use the zero-downtime
   upgrade path (SIGQUIT/`GracefulUpgrade` + fd transfer) — both assume Pingora
   owns the process lifecycle.

Net: signal ownership stays with Denia. GREEN, embedding model is viable.

---

## Spike 0.2 — UDS upstream — VERDICT: YES

**Question:** can `HttpPeer` dial a Unix-domain-socket upstream (not TCP)?

**Answer: YES, first-class.** Exact constructor:
```rust
// pingora-core-0.8.0/src/upstreams/peer.rs:620
pub fn HttpPeer::new_uds(path: &str, tls: bool, sni: String) -> Result<HttpPeer>
```
It stores `SocketAddr::Unix(UnixSocketAddr::from_pathname(path))`. The L4
connector genuinely dials it — `connectors/l4.rs:158` matches
`SocketAddr::Unix(addr) => connect_uds(addr.as_pathname()...)`. (There is also
`BasicPeer::new_uds` for non-HTTP peers.)

Usage inside the proxy:
```rust
async fn upstream_peer(&self, _s: &mut Session, _c: &mut ()) -> Result<Box<HttpPeer>> {
    let peer = HttpPeer::new_uds("/run/denia/app.sock", false, "upstream".into())?;
    Ok(Box::new(peer))
}
```

**Runtime evidence** (spike02_uds — client over TCP -> proxy -> UDS upstream):
```
=== proxy response via UDS upstream ===
HTTP/1.1 200 OK
hello-from-uds
=======================================
SPIKE 0.2 RESULT: YES - UDS upstream works end to end.
```

**Impact:** the loopback-TCP bridge layer Denia currently needs for Traefik
compatibility can be **deleted** for the Pingora data path. Workloads can be
addressed directly by their Denia-owned Unix socket. (One caveat to confirm
later: non-pathname/abstract sockets are explicitly unsupported —
`connect_uds` calls `.as_pathname().expect(...)`, so stick to filesystem-path
UDS.)

---

## Spike 0.3 — Dynamic per-SNI cert callback — VERDICT: works (boringssl/openssl only)

**Question:** exact API to serve a TLS cert chosen at handshake time from
in-memory state, and behavior when no cert exists for the SNI.

**Exact trait + signatures** (`pingora-core-0.8.0/src/listeners/mod.rs:95`):
```rust
#[async_trait]
pub trait TlsAccept {
    // Called mid-handshake. Implementor installs cert+key on the ssl via
    // ext::ssl_use_certificate / ext::ssl_use_private_key. Returns ().
    // NOTE: only supported on openssl and boringssl backends (NOT rustls).
    async fn certificate_callback(&self, _ssl: &mut TlsRef) -> () { /* default no-op */ }

    // Called after handshake; optional opaque value stored on the SslDigest.
    async fn handshake_complete_callback(&self, _ssl: &TlsRef)
        -> Option<Arc<dyn Any + Send + Sync>> { None }
}
pub type TlsAcceptCallbacks = Box<dyn TlsAccept + Send + Sync>;
```
Wiring it onto a listener:
```rust
// pingora-core-0.8.0/src/listeners/tls/boringssl_openssl/mod.rs:92
TlsSettings::with_callbacks(callbacks: TlsAcceptCallbacks) -> Result<TlsSettings>
// then on the proxy Service (services/listening.rs:146):
proxy.add_tls_with_settings("0.0.0.0:443", None, tls_settings);
```
`TlsRef` is `pingora::protocols::tls::TlsRef` = `SslRef`. Read SNI inside the
callback with `ssl.servername(NameType::HOST_NAME)`. Install the chosen cert with:
```rust
use pingora::tls::ext;
ext::ssl_use_certificate(ssl, &entry.cert)?;   // X509
ext::ssl_use_private_key(ssl, &entry.key)?;    // PKey<Private>
```

In-memory hot-swap store that worked (`ArcSwap<HashMap<String, CertEntry>>`):
```rust
struct SniResolver { store: Arc<ArcSwap<HashMap<String, CertEntry>>> }
#[async_trait]
impl TlsAccept for SniResolver {
    async fn certificate_callback(&self, ssl: &mut TlsRef) {
        let sni = ssl.servername(NameType::HOST_NAME).map(str::to_string).unwrap_or_default();
        match self.store.load().get(&sni) {
            Some(e) => { ext::ssl_use_certificate(ssl, &e.cert).unwrap();
                         ext::ssl_use_private_key(ssl, &e.key).unwrap(); }
            None    => { /* install nothing -> handshake fails cleanly */ }
        }
    }
}
```

**Decline behavior (the important part):** if `certificate_callback` installs no
certificate, the handshake-resume path
(`protocols/tls/boringssl_openssl/server.rs:handshake_with_callback` ->
`resume_accept`) fails the TLS `accept()` and returns
`Err(TLSHandshakeFailure)`. **No panic, no fallback/default cert, no
wrong-cert leak** — the client simply gets a failed handshake (observed as
`unexpected EOF` on the probe). The mechanism: `start_accept` arms
`suspend_when_need_ssl_cert`; with no cert provided, resume cannot complete.

**Runtime evidence** (spike03_cert, process exit 0):
```
--- TEST 1: known SNI alpha.test (should succeed) ---
[cert-cb] served cert for SNI="alpha.test"
Ok("handshake OK, peer cert CN=Some(\"alpha.test\")")
--- TEST 2: unknown SNI ghost.test (should DECLINE/fail cleanly) ---
[cert-cb] DECLINE: no cert for SNI="ghost.test" (handshake will fail)
Err("handshake FAILED: unexpected EOF")
--- TEST 3: beta.test after live swap (should succeed) ---
[cert-cb] served cert for SNI="beta.test"
Ok("handshake OK, peer cert CN=Some(\"beta.test\")")
SPIKE 0.3: completed without process crash.
```
TEST 3 added `beta.test` to the `ArcSwap` at runtime with **no listener
restart** and it was served on the very next connection — confirms live cert
rotation works.

**Backend constraint:** dynamic cert selection requires `boringssl` or
`openssl`. The `certificate_callback` / `ext::ssl_use_*` path is documented and
implemented only for those backends; on `rustls`/`s2n`/no-tls it is a stub. This
forces Denia onto the boringssl (or openssl) feature — which is fine, boringssl
is the default and builds cleanly here.

---

## Final design-viability call: GO

All three gating risks clear:

- **0.1 GREEN** — Pingora cedes signal ownership via a custom
  `ShutdownSignalWatch` in `RunArgs`; run it on a dedicated thread with
  `run()` (never `run_forever()`). Denia keeps authoritative process control.
- **0.2 YES** — `HttpPeer::new_uds()` dials filesystem UDS upstreams directly;
  the loopback-TCP bridge can be removed from the data path.
- **0.3 YES** — `TlsAccept::certificate_callback` + `ext::ssl_use_certificate`
  give per-SNI in-memory cert selection with clean decline (failed handshake,
  no leak) and live hot-swap via `ArcSwap`. Requires the `boringssl` feature.

No build or system-dependency blockers in this environment. The in-process
Pingora ingress design is viable; proceed to a full design/ADR.

### Follow-ups to lock down in design phase
- Confirm abstract-namespace UDS is not needed (only pathname UDS is supported).
- Decide boringssl vs openssl feature (boringssl default; openssl uses system lib).
- Map Pingora's startup `process::exit(1)` fault paths to a Denia supervisor policy.
- Validate behavior under Denia's existing namespace/userns isolation for the
  Pingora thread (it spawns its own tokio runtimes/threads).

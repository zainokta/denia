# ADR-040: Opt-In DNS-01 ACME via Pluggable Providers (Cloudflare + exec)

- Status: Accepted
- Date: 2026-06-05

## Context

Denia issues TLS certificates in-process with `instant-acme` using the **HTTP-01**
challenge: Let's Encrypt fetches `http://<host>/.well-known/acme-challenge/<token>`
from `:80`, which Denia's Pingora ingress serves from the control backend
(ADR-007, ADR-020). HTTP-01 requires Let's Encrypt to reach the origin directly.

A **Cloudflare proxied (orange-cloud)** record does not by itself break HTTP-01.
In SSL mode Full or Full (strict), Cloudflare forwards a plain-HTTP request to
the origin's `:80` (it uses the scheme the visitor used), and Denia's `:80`
listener answers `/.well-known/acme-challenge/` and `/.well-known/denia-challenge/`
before its own HTTPS redirect. HTTP-01 behind Cloudflare works with no DNS
credential once the edge passes those paths through: Always Use HTTPS off (or a
redirect rule that skips them, since Denia's domain verifier does not follow
redirects), Bot Fight Mode off, and a WAF Skip rule for the challenge paths.
This is the documented default path ("Domains behind Cloudflare" guide).

HTTP-01 still cannot cover every case:

- The operator cannot or will not change those edge settings (Bot Fight Mode
  cannot be bypassed by WAF rules on the Free plan; SSL-Only Origin Pull sends
  the challenge to `:443` before a cert exists).
- `:80` is not reachable from the internet at all (firewall policy, a WAF that
  terminates `:80`, a private origin behind a tunnel).
- **Wildcard** certificates, which HTTP-01 cannot issue.

For these, Dokploy/Traefik use the DNS-01 challenge: prove domain ownership by
writing a `_acme-challenge.<domain>` TXT record via the DNS provider's API, which
needs no inbound reachability to the origin.

## Decision

Add an **opt-in DNS-01 challenge with pluggable providers**, modeled on
Traefik/lego. HTTP-01 remains the default; nothing changes unless DNS-01 is
configured.

**Provider trait (lego's `challenge.Provider`).** `Dns01Provider` (in
`src/ingress/pingora/dns01/`) is the per-provider contract:
`present(fqdn, value) -> TxtHandle` (create the TXT record) + `cleanup(handle)`
(idempotent delete) + an optional `timeout() -> (timeout, interval)` for
propagation tuning. The factory `build_provider(config)` maps
`DENIA_ACME_DNS_PROVIDER` → an `Arc<dyn Dns01Provider>`. Adding a provider is one
trait impl + one `DnsProviderKind` variant — the core stays provider-agnostic.

**Solver abstraction.** `AcmeDriver` holds a `ChallengeSolver` enum
(`Http01(ChallengeStore)` | `Dns01 { provider, propagation }`). `AcmeDriver::issue`
keeps the order setup (`new_order`) and finalize tail (`finalize_order`) identical
for both; only how each authorization is answered branches. All downstream
machinery — `persist_cert`, `load_certs_from_disk`, `select_renewals`, the 12h
renewal scan, the on-demand issue channel, and boot/control-domain issuance in
`daemon.rs` — is unchanged and challenge-agnostic. TXT records are cleaned up via
`provider.cleanup` on every exit path (success and error), because `Drop` cannot
await.

**Generic propagation + CNAME delegation** (`dns01/propagation.rs`). Propagation
confirmation is provider-agnostic (lego keeps it generic; providers only tune
`timeout()`): after an initial delay it polls DoH resolvers until the TXT is
visible, or times out — `set_ready` never fires on an unconfirmed record. It also
resolves a CNAME on `_acme-challenge.<domain>` to an **effective FQDN**, so a
supported provider can answer challenges for a domain whose apex DNS lives
elsewhere (delegate `_acme-challenge` into a zone you control).

**Providers shipped.**
- `cloudflare` (`dns01/cloudflare.rs`) — Cloudflare API v4 client (`reqwest`,
  already a dependency). Resolves the owning zone via `GET /zones?name=`
  most-specific-first (no public-suffix-list dep), creates/deletes the TXT, and
  classifies API errors retryable (transport, 429, 5xx) vs permanent (401/403
  auth, unknown zone). Token held in a `Secret` newtype (redacted `Debug`).
- `exec` (`dns01/exec.rs`) — runs `<script> present|cleanup <fqdn> <value>`
  (`tokio::process`). Covers any DNS host (Akamai, Route53, PowerDNS, BIND, …).
  Denia passes only the non-secret fqdn+value; the script owns its own
  credentials, so no provider secret is held by Denia. Hook stdin, stdout, and
  stderr are always disconnected; output is never retained or logged. Each hook
  operation has a 30-second deadline, after which Denia kills and reaps it.
- Cloudflare zone lookup has a 30-second deadline across all suffix candidates;
  each individual request, TXT creation, and TXT cleanup also has a 30-second
  deadline. Transport timeouts are retryable; exec hook timeouts are reported
  as retryable provider errors.

**Configuration.**

- `DENIA_ACME_CHALLENGE` = `http-01` (default) | `dns-01`.
- `DENIA_ACME_DNS_PROVIDER` = `cloudflare` (default) | `exec`.
- `DENIA_CF_DNS_API_TOKEN` — Cloudflare token with `Zone:Read` + `DNS:Edit`. A
  **secret**: env var (systemd `EnvironmentFile`) or a `cf_dns_api_token_file`
  path; **never** raw in `config.toml`, never logged (`Secret` newtype).
- `DENIA_ACME_DNS_EXEC` — path to the `exec` provider script (not a secret).
- `DENIA_ACME_DNS_PROPAGATION_SECS` (default 60) — delay before the first check.
- `DENIA_ACME_DNS_RESOLVERS` — DoH endpoints for propagation + CNAME resolution;
  empty → built-in public resolvers (Cloudflare + Google).

`AppConfig::require_dns01_provider` fails startup whenever DNS-01 is selected and
the chosen provider's config is missing (cloudflare → token; exec → script path),
regardless of whether TLS is in use yet — a misconfigured `dns-01` fails fast at
boot rather than silently disabling issuance. A Cloudflare token configured under
HTTP-01 is logged as inert.

**Scope.** v1 issues single-host certificates (the orange-cloud control-console
case). Wildcard issuance — which additionally requires allowing a leading `*.` at
the ACME-order-identifier boundary and a filesystem-safe cert-directory name — is
a documented follow-up; the trait + per-authorization loop leave room for it.

## Consequences

- Easier: Denia issues and auto-renews Let's Encrypt certs for hostnames whose
  `:80` is unreachable or whose edge blocks the challenge, on **any** DNS host (Cloudflare natively, anything else via `exec` or
  CNAME delegation), and the design is ready to extend to wildcard certs.
- Easier: pairs cleanly with Cloudflare SSL mode **Full (strict)** — LE certs are
  trusted by Cloudflare without an Origin CA cert.
- Harder: DNS-01 requires a Cloudflare API token with `DNS:Edit`; the token is a
  new secret to provision and protect. The provider writes/deletes DNS records, so
  a wrong/over-scoped token is a larger blast radius than HTTP-01.
- Harder: issuance now depends on DNS propagation timing; the propagation delay
  (and optional DoH check) trade latency for reliability.
- Note: the Cloudflare edge can still return 403 to API clients via Bot Fight Mode
  / WAF / Access — that is an edge policy concern, independent of certificates, and
  is resolved on the Cloudflare side (disable Bot Fight Mode, which Skip rules
  cannot bypass, and add a WAF Skip rule for `/v1/*` and `/healthz`).

## Alternatives Considered

- **HTTP-01 through the Cloudflare proxy, no DNS-01.** Needs no credential and
  no code: configure the edge as described in Context. This stays the default
  and the recommended path for orange-cloud hostnames. Not sufficient alone
  because it depends on edge settings the operator may not control, needs `:80`
  reachable, and cannot issue wildcards, so DNS-01 is added as an opt-in rather
  than a replacement.
- **HTTP-01 only + a Cloudflare Origin CA certificate** dropped into `<tls_dir>`.
  Works today (Denia boot-loads any cert and skips ACME when one exists) but the
  cert is operator-managed with no automatic renewal, and Origin CA certs are
  trusted only by Cloudflare. Rejected as the primary path; still available as a
  manual workaround.
- **Cloudflare Tunnel** (`cloudflared`). Avoids inbound ports entirely but adds an
  external daemon and moves TLS termination off Denia, in tension with ADR-020
  ("Denia owns `:80`/`:443`"). Out of scope; may be documented as a deployment
  option.
- **Native providers for every DNS host** (Route53, Akamai EdgeGrid, …) like
  lego's 200+ catalogue. Each is a bespoke API client to maintain. Rejected for
  now: the `exec` provider covers any host with one script, and CNAME delegation
  lets the Cloudflare provider answer for zones it doesn't host. A native provider
  can still be added later as one `Dns01Provider` impl when demand justifies it.
- **A single hardcoded Cloudflare provider** (the v1 of this ADR). Superseded:
  operators on other DNS hosts had no path. The trait + `exec` generalize it
  without losing the Cloudflare fast path.

## References

- ADR-007 (Ingress + TLS), ADR-020 (In-Process Pingora Ingress), ADR-035 (Control
  Domain Over Ingress)
- ADR-023 (TOML config + env override — the secret-sourcing pattern)
- `instant-acme` DNS-01: <https://docs.rs/instant-acme/>
- Cloudflare API (DNS records): <https://developers.cloudflare.com/api/>
- Denia docs, Domains behind Cloudflare (HTTP-01 edge settings):
  <https://github.com/zainokta/denia-documentation/blob/master/content/guides/domains-behind-cloudflare.md>
- Cloudflare SSL mode Full (scheme matches visitor): <https://developers.cloudflare.com/ssl/origin-configuration/ssl-modes/full/>
- Cloudflare Bot Fight Mode (not skippable by WAF rules): <https://developers.cloudflare.com/bots/get-started/bot-fight-mode/>

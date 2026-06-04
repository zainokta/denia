#!/usr/bin/env bash
# Denia installer — build-only.
#
# Installs build prerequisites, builds the release binary + embedded SPA via
# `make install`, and prints the next step: `sudo denia setup`.
#
# Provisioning (user, layout, age key, admin token, config, systemd unit) is
# handled by `denia setup` so it always tracks the daemon version.

set -euo pipefail
IFS=$'\n\t'

# ----------------------------------------------------------------------------
# Constants / paths
# ----------------------------------------------------------------------------

readonly DENIA_BIN="/usr/local/bin/denia"
readonly DENIA_BUILD_HOME="/usr/local/src/denia-build"

readonly RUSTUP_INIT_URL="https://sh.rustup.rs"

# Node 22 LTS; pnpm is bootstrapped via corepack which ships with Node >= 16.
readonly NODE_MAJOR="22"
readonly NODESOURCE_SETUP_URL="https://deb.nodesource.com/setup_${NODE_MAJOR}.x"
readonly PNPM_VERSION="10.25.0"
readonly SOPS_VERSION="${DENIA_SOPS_VERSION:-3.13.1}"
readonly BUILDKIT_VERSION="${DENIA_BUILDKIT_VERSION:-0.30.0}"
readonly MIN_GLIBC_MAJOR="2"
readonly MIN_GLIBC_MINOR="39"

# ----------------------------------------------------------------------------
# CLI flags
# ----------------------------------------------------------------------------

DRY_RUN=0
SKIP_BUILD=0

usage() {
    cat <<'EOF'
Usage: install.sh [--dry-run] [--skip-build]

Options:
  --dry-run      Print every command that would run, change nothing.
  --skip-build   Skip cargo/pnpm/make. Installs from ./target/release/denia
                 if it exists; otherwise runs make install (cargo is incremental).
  -h, --help     Show this help.

Provisioning (user, layout, age key, admin token, config, systemd unit) is a
separate step handled by the binary:

  sudo denia setup

To uninstall, use:

  sudo denia uninstall [--purge]
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run)        DRY_RUN=1 ;;
        --skip-build)     SKIP_BUILD=1 ;;
        --uninstall|--purge)
            printf '  [XX] install.sh no longer handles uninstall. Use: sudo denia uninstall [--purge]\n' >&2
            exit 1
            ;;
        -h|--help)        usage; exit 0 ;;
        *) echo "unknown flag: $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

# ----------------------------------------------------------------------------
# Logging
# ----------------------------------------------------------------------------

if [[ -t 1 ]]; then
    C_GREEN=$'\033[32m'
    C_RED=$'\033[31m'
    C_YELLOW=$'\033[33m'
    C_BOLD=$'\033[1m'
    C_RESET=$'\033[0m'
else
    C_GREEN=""; C_RED=""; C_YELLOW=""; C_BOLD=""; C_RESET=""
fi

step() {
    printf '%s==>%s %s%s%s\n' "${C_BOLD}" "${C_RESET}" "${C_BOLD}" "$1" "${C_RESET}"
}

ok() {
    printf '  %s[OK]%s %s\n' "${C_GREEN}" "${C_RESET}" "$1"
}

warn() {
    printf '  %s[!!]%s %s\n' "${C_YELLOW}" "${C_RESET}" "$1" >&2
}

fail() {
    printf '  %s[XX]%s %s\n' "${C_RED}" "${C_RESET}" "$1" >&2
    exit 1
}

# run_cmd: execute, or under --dry-run just print. Quote every callsite.
run_cmd() {
    if [[ "${DRY_RUN}" -eq 1 ]]; then
        printf '  %s[dry-run]%s %s\n' "${C_YELLOW}" "${C_RESET}" "$*"
        return 0
    fi
    "$@"
}

# run_sh: same as run_cmd but takes a single shell string for pipes/redirects.
# Used sparingly; prefer run_cmd.
run_sh() {
    if [[ "${DRY_RUN}" -eq 1 ]]; then
        printf '  %s[dry-run]%s sh -c %q\n' "${C_YELLOW}" "${C_RESET}" "$1"
        return 0
    fi
    /bin/sh -c "$1"
}

# ----------------------------------------------------------------------------
# Preflight: OS / arch / root / kernel features / port conflicts
# ----------------------------------------------------------------------------

step_preflight_os() {
    step "Preflight: OS, architecture, root"

    if [[ "$(uname -s)" != "Linux" ]]; then
        fail "Denia requires Linux. Detected: $(uname -s)."
    fi

    local arch
    arch="$(uname -m)"
    case "${arch}" in
        x86_64|aarch64|arm64) ok "architecture: ${arch}" ;;
        *) fail "Unsupported architecture: ${arch}. Need x86_64 or arm64." ;;
    esac

    # Reject WSL: it lacks the kernel surface needed for namespaces+cgroup v2
    # reliably and tends to multiplex networking through the host.
    if [[ -r /proc/version ]] && /bin/grep -qiE "microsoft|wsl" /proc/version; then
        fail "WSL detected. Denia needs a real Linux kernel; install on bare metal, a VM, or a Linux cloud host."
    fi

    # Release binaries are built on Ubuntu 24.04, so the supported production
    # host contract is glibc 2.39+. Source builds on older hosts may work, but
    # `denia update` would later fetch a signed binary that cannot exec there.
    local ldd_path ldd_out glibc_version glibc_major glibc_minor
    ldd_path="$(command -v ldd || true)"
    if [[ -z "${ldd_path}" ]]; then
        fail "ldd not found. Denia requires a glibc host with glibc >= ${MIN_GLIBC_MAJOR}.${MIN_GLIBC_MINOR}."
    fi
    ldd_out="$("${ldd_path}" --version 2>&1 || true)"
    if ! printf '%s\n' "${ldd_out}" | /bin/grep -qiE "glibc|gnu libc"; then
        fail "Non-glibc libc detected (likely musl). Denia requires glibc >= ${MIN_GLIBC_MAJOR}.${MIN_GLIBC_MINOR}."
    fi
    glibc_version="$(printf '%s\n' "${ldd_out}" | /bin/grep -Eo '[0-9]+\.[0-9]+' | /usr/bin/head -n1 || true)"
    if [[ -z "${glibc_version}" ]]; then
        fail "Could not parse glibc version from ldd output. Denia requires glibc >= ${MIN_GLIBC_MAJOR}.${MIN_GLIBC_MINOR}."
    fi
    glibc_major="${glibc_version%%.*}"
    glibc_minor="${glibc_version#*.}"
    if (( glibc_major < MIN_GLIBC_MAJOR || (glibc_major == MIN_GLIBC_MAJOR && glibc_minor < MIN_GLIBC_MINOR) )); then
        fail "glibc ${glibc_version} is below Denia's required ${MIN_GLIBC_MAJOR}.${MIN_GLIBC_MINOR} baseline.
  Release binaries are built for Ubuntu 24.04-era glibc so signed self-updates can run.
  Remediation: upgrade the host OS before installing Denia."
    fi
    ok "glibc ${glibc_version} meets release baseline >= ${MIN_GLIBC_MAJOR}.${MIN_GLIBC_MINOR}"

    if [[ "${EUID}" -ne 0 ]]; then
        if [[ "${DRY_RUN}" -eq 1 ]]; then
            warn "not running as root (--dry-run; would require sudo in production)"
        else
            fail "Must run as root (need /usr/local/bin). Try: sudo $0"
        fi
    else
        ok "running as root"
    fi
}

step_preflight_kernel() {
    step "Preflight: kernel features (cgroup v2, user namespaces)"

    # cgroup v2 unified hierarchy. /proc/mounts must show exactly one cgroup2
    # mount at /sys/fs/cgroup, and no legacy `cgroup` (v1) mounts there.
    if ! /bin/grep -qE '^cgroup2 /sys/fs/cgroup cgroup2 ' /proc/mounts; then
        fail "cgroup v2 unified hierarchy not mounted at /sys/fs/cgroup.
  Remediation: boot with systemd.unified_cgroup_hierarchy=1 (kernel cmdline)
  or remove the legacy hybrid setup. See https://systemd.io/CGROUP_DELEGATION/"
    fi
    local cg2_count
    cg2_count="$(/bin/grep -cE '^cgroup2 ' /proc/mounts || true)"
    if [[ "${cg2_count}" -ne 1 ]]; then
        fail "Expected exactly one cgroup2 mount, found ${cg2_count}. Hybrid/legacy v1 mounts are unsupported."
    fi
    ok "cgroup v2 mounted at /sys/fs/cgroup"

    # User namespaces: required for the workload launcher.
    local maxns="0"
    if [[ -r /proc/sys/user/max_user_namespaces ]]; then
        maxns="$(/bin/cat /proc/sys/user/max_user_namespaces || echo 0)"
    fi
    if [[ "${maxns}" -le 0 ]]; then
        fail "user namespaces disabled (/proc/sys/user/max_user_namespaces=${maxns}).
  Remediation: sysctl -w user.max_user_namespaces=15000 and persist in /etc/sysctl.d/."
    fi
    ok "user namespaces enabled (max=${maxns})"

    # Debian-family: unprivileged_userns_clone must be 1.
    if [[ -r /etc/debian_version ]] && [[ -r /proc/sys/kernel/unprivileged_userns_clone ]]; then
        local uunc
        uunc="$(/bin/cat /proc/sys/kernel/unprivileged_userns_clone || echo 0)"
        if [[ "${uunc}" -ne 1 ]]; then
            warn "kernel.unprivileged_userns_clone=${uunc} on a Debian-family host.
  Denia itself runs as root with CAP_SYS_ADMIN so it can still unshare, but
  set 'kernel.unprivileged_userns_clone=1' in /etc/sysctl.d/ for parity with
  upstream guidance and to allow rootless smoke tests."
        else
            ok "unprivileged_userns_clone=1"
        fi
    fi
}

step_preflight_ports() {
    step "Preflight: :80 / :443 not already bound"

    # Denia *owns* :80 and :443 via Pingora (ADR-020). If anything else is
    # already listening on the host, refuse — the operator must not run a
    # separate Traefik/nginx/Caddy alongside.
    if ! command -v ss >/dev/null 2>&1; then
        warn "iproute2 'ss' not installed; skipping port-conflict check. Install iproute2 to enforce."
        return 0
    fi

    local conflicts
    conflicts="$(ss -ltnH '( sport = :80 or sport = :443 )' 2>/dev/null || true)"
    if [[ -n "${conflicts}" ]]; then
        printf '%s\n' "${conflicts}" >&2
        if [[ "${DRY_RUN}" -eq 1 ]]; then
            warn ":80 or :443 already bound (--dry-run; would fail in production).
  Denia owns these ports (Pingora; see docs/adr/020-pingora-ingress.md)."
        else
            fail "Something is already listening on :80 or :443.
  Denia owns these ports (Pingora; see docs/adr/020-pingora-ingress.md).
  Remediation: stop and disable the offending service (Traefik, nginx, Caddy,
  Apache, ...) before re-running this installer."
        fi
    else
        ok ":80 and :443 are free"
    fi
}

# ----------------------------------------------------------------------------
# Package manager detection + install prerequisites
# ----------------------------------------------------------------------------

PKG_MGR=""

detect_pkg_mgr() {
    if   command -v apt-get >/dev/null 2>&1; then PKG_MGR="apt"
    elif command -v dnf     >/dev/null 2>&1; then PKG_MGR="dnf"
    elif command -v pacman  >/dev/null 2>&1; then PKG_MGR="pacman"
    elif command -v zypper  >/dev/null 2>&1; then PKG_MGR="zypper"
    else fail "no supported package manager (apt/dnf/pacman/zypper) found"
    fi
}

step_install_prereqs() {
    step "Install build prerequisites (no Docker)"
    detect_pkg_mgr
    ok "package manager: ${PKG_MGR}"

    # Refuse if docker.service is running as the workload runtime — Denia is
    # explicitly Docker-free. Docker on the host for other reasons is fine, but
    # it must not own :80/:443 or override cgroup delegation.
    if command -v systemctl >/dev/null 2>&1 && systemctl is-active --quiet docker 2>/dev/null; then
        warn "dockerd is running on this host. Denia does not use Docker as the workload
  runtime; ensure no Docker container holds :80/:443 or steals cgroup v2 root."
    fi

    case "${PKG_MGR}" in
        apt)
            run_cmd env DEBIAN_FRONTEND=noninteractive apt-get update -y
            run_cmd env DEBIAN_FRONTEND=noninteractive apt-get install -y \
                build-essential pkg-config libssl-dev libclang-dev clang cmake perl ca-certificates \
                curl git age acl iproute2 procps runc
            ;;
        dnf)
            run_cmd dnf install -y \
                @development-tools pkgconf-pkg-config openssl-devel clang clang-devel cmake perl ca-certificates \
                curl git age acl iproute procps-ng runc
            ;;
        pacman)
            run_cmd pacman -Sy --noconfirm \
                base-devel pkgconf openssl clang cmake perl ca-certificates \
                curl git age acl iproute2 procps-ng runc
            ;;
        zypper)
            run_cmd zypper --non-interactive refresh
            run_cmd zypper --non-interactive install -y \
                -t pattern devel_basis
            run_cmd zypper --non-interactive install -y \
                pkg-config libopenssl-devel clang clang-devel cmake perl ca-certificates \
                curl git age acl iproute2 procps runc
            ;;
    esac
    ok "system packages installed"
}

# ----------------------------------------------------------------------------
# Runtime/build host tools: SOPS + BuildKit
# ----------------------------------------------------------------------------

linux_arch() {
    case "$(uname -m)" in
        x86_64) echo "amd64" ;;
        aarch64|arm64) echo "arm64" ;;
        *) fail "unsupported architecture for release tools: $(uname -m)" ;;
    esac
}

verify_optional_sha256() {
    local path="$1"
    local expected="$2"
    local label="$3"
    if [[ -z "${expected}" ]]; then
        return 0
    fi
    local actual
    actual="$(/usr/bin/sha256sum "${path}" | /usr/bin/awk '{print $1}')"
    if [[ "${actual}" != "${expected}" ]]; then
        fail "${label} sha256 mismatch: got ${actual}, expected ${expected}"
    fi
    ok "${label} sha256 verified"
}

step_install_sops() {
    step "Install SOPS ${SOPS_VERSION}"

    if command -v sops >/dev/null 2>&1; then
        ok "sops already present: $(command -v sops)"
        return 0
    fi

    local arch tmp url
    arch="$(linux_arch)"
    url="https://github.com/getsops/sops/releases/download/v${SOPS_VERSION}/sops-v${SOPS_VERSION}.linux.${arch}"
    tmp="$(/usr/bin/mktemp)"
    trap "rm -f '${tmp}'" RETURN

    run_cmd /usr/bin/curl --proto '=https' --tlsv1.2 -fsSL "${url}" -o "${tmp}"
    if [[ "${DRY_RUN}" -eq 0 ]]; then
        verify_optional_sha256 "${tmp}" "${DENIA_SOPS_SHA256:-}" "sops"
    fi
    run_cmd install -Dm0755 "${tmp}" /usr/local/bin/sops
    ok "sops installed at /usr/local/bin/sops"
}

step_install_buildkit() {
    step "Install BuildKit ${BUILDKIT_VERSION}"

    if command -v buildctl >/dev/null 2>&1 && command -v buildkitd >/dev/null 2>&1; then
        ok "buildkit already present: $(command -v buildctl), $(command -v buildkitd)"
        return 0
    fi

    local arch tmp workdir url
    arch="$(linux_arch)"
    tmp="$(/usr/bin/mktemp)"
    workdir="$(/usr/bin/mktemp -d)"
    trap "rm -f '${tmp}'; rm -rf '${workdir}'" RETURN
    url="https://github.com/moby/buildkit/releases/download/v${BUILDKIT_VERSION}/buildkit-v${BUILDKIT_VERSION}.linux-${arch}.tar.gz"

    run_cmd /usr/bin/curl --proto '=https' --tlsv1.2 -fsSL "${url}" -o "${tmp}"
    if [[ "${DRY_RUN}" -eq 0 ]]; then
        verify_optional_sha256 "${tmp}" "${DENIA_BUILDKIT_SHA256:-}" "buildkit"
        run_cmd /bin/tar -C "${workdir}" -xzf "${tmp}"
    else
        run_cmd /bin/tar -C "${workdir}" -xzf "${tmp}"
    fi
    run_cmd install -Dm0755 "${workdir}/bin/buildctl" /usr/local/bin/buildctl
    run_cmd install -Dm0755 "${workdir}/bin/buildkitd" /usr/local/bin/buildkitd
    ok "buildctl/buildkitd installed under /usr/local/bin"
}

# ----------------------------------------------------------------------------
# Rust toolchain (isolated CARGO_HOME)
# ----------------------------------------------------------------------------

step_install_rust() {
    step "Install Rust toolchain (isolated under ${DENIA_BUILD_HOME})"

    if [[ "${SKIP_BUILD}" -eq 1 ]]; then
        ok "skipped (--skip-build)"
        return 0
    fi

    run_cmd install -d -m 0755 "${DENIA_BUILD_HOME}"

    export CARGO_HOME="${DENIA_BUILD_HOME}/cargo"
    export RUSTUP_HOME="${DENIA_BUILD_HOME}/rustup"
    export PATH="${CARGO_HOME}/bin:${PATH}"

    if [[ -x "${CARGO_HOME}/bin/cargo" ]]; then
        ok "rust toolchain already present at ${CARGO_HOME}"
        return 0
    fi

    # rustup-init: fetch to a tempfile, verify against an operator-pinned
    # SHA256, then execute with explicit flags. No `curl | sh`.
    if [[ -z "${DENIA_RUSTUP_SHA256:-}" ]]; then
        fail "DENIA_RUSTUP_SHA256 must be set before installing Rust via rustup.
  Re-run with DENIA_RUSTUP_SHA256=<known-good sha256 for ${RUSTUP_INIT_URL}>,
  or preinstall cargo at ${CARGO_HOME}/bin/cargo."
    fi
    local tmp
    tmp="$(/usr/bin/mktemp)"
    # shellcheck disable=SC2064 # we want $tmp expanded now, this trap is local
    trap "rm -f '${tmp}'" RETURN

    run_cmd /usr/bin/curl --proto '=https' --tlsv1.2 -sSfL "${RUSTUP_INIT_URL}" -o "${tmp}"

    local actual
    actual="$(/usr/bin/sha256sum "${tmp}" | /usr/bin/awk '{print $1}')"
    if [[ "${actual}" != "${DENIA_RUSTUP_SHA256}" ]]; then
        fail "rustup-init sha256 mismatch: got ${actual}, expected ${DENIA_RUSTUP_SHA256}"
    fi
    ok "rustup-init sha256 verified"

    run_cmd /bin/sh "${tmp}" --default-toolchain stable --profile minimal -y \
        --no-modify-path

    if [[ ! -x "${CARGO_HOME}/bin/cargo" ]]; then
        if [[ "${DRY_RUN}" -eq 0 ]]; then
            fail "rustup did not install cargo at ${CARGO_HOME}/bin/cargo"
        fi
    fi
    ok "rust stable installed"
}

# ----------------------------------------------------------------------------
# Node 22 + pnpm
# ----------------------------------------------------------------------------

step_install_node() {
    step "Install Node ${NODE_MAJOR} + pnpm (corepack)"

    if [[ "${SKIP_BUILD}" -eq 1 ]]; then
        ok "skipped (--skip-build)"
        return 0
    fi

    local node_major_found="0"
    if command -v node >/dev/null 2>&1; then
        node_major_found="$(node -v 2>/dev/null | /bin/sed -E 's/^v([0-9]+).*/\1/')"
    fi

    if [[ "${node_major_found}" != "${NODE_MAJOR}" ]]; then
        case "${PKG_MGR}" in
            apt)
                # NodeSource provides a Node 22 LTS apt repo. Fetched as a script,
                # then executed from a fresh mktemp path so another local user
                # cannot pre-place or swap a predictable /tmp script.
                local nodesource_setup
                if [[ "${DRY_RUN}" -eq 1 ]]; then
                    nodesource_setup="/tmp/denia-nodesource.XXXXXX"
                else
                    nodesource_setup="$(mktemp -t denia-nodesource.XXXXXX)"
                    trap 'rm -f "${nodesource_setup:-}"' RETURN
                fi
                if [[ -z "${DENIA_NODESOURCE_SETUP_SHA256:-}" ]]; then
                    fail "DENIA_NODESOURCE_SETUP_SHA256 must be set before executing ${NODESOURCE_SETUP_URL}.
  Re-run with DENIA_NODESOURCE_SETUP_SHA256=<known-good sha256>, or preinstall Node ${NODE_MAJOR}."
                fi
                run_cmd /usr/bin/curl --proto '=https' --tlsv1.2 -fsSL "${NODESOURCE_SETUP_URL}" -o "${nodesource_setup}"
                local nodesource_actual
                nodesource_actual="$(/usr/bin/sha256sum "${nodesource_setup}" | /usr/bin/awk '{print $1}')"
                if [[ "${nodesource_actual}" != "${DENIA_NODESOURCE_SETUP_SHA256}" ]]; then
                    fail "NodeSource setup sha256 mismatch: got ${nodesource_actual}, expected ${DENIA_NODESOURCE_SETUP_SHA256}"
                fi
                ok "NodeSource setup sha256 verified"
                run_cmd /bin/bash "${nodesource_setup}"
                run_cmd env DEBIAN_FRONTEND=noninteractive apt-get install -y nodejs
                run_cmd rm -f "${nodesource_setup}"
                nodesource_setup=""
                ;;
            dnf)
                run_cmd dnf module reset -y nodejs || true
                run_cmd dnf module enable -y "nodejs:${NODE_MAJOR}" || true
                run_cmd dnf install -y nodejs npm
                ;;
            pacman)
                run_cmd pacman -Sy --noconfirm nodejs npm
                ;;
            zypper)
                run_cmd zypper --non-interactive install -y "nodejs${NODE_MAJOR}" "npm${NODE_MAJOR}" || \
                    run_cmd zypper --non-interactive install -y nodejs npm
                ;;
        esac
    else
        ok "node ${NODE_MAJOR} already present"
    fi

    # Enable corepack (ships with Node) for a pinned, repo-local pnpm.
    if command -v corepack >/dev/null 2>&1; then
        run_cmd corepack enable
        run_cmd corepack prepare "pnpm@${PNPM_VERSION}" --activate
        ok "pnpm via corepack"
    else
        run_cmd npm install -g "pnpm@${PNPM_VERSION}"
        ok "pnpm via npm -g (corepack unavailable)"
    fi
}

# ----------------------------------------------------------------------------
# Build + install via make
# ----------------------------------------------------------------------------

step_make_install() {
    step "Build + install binary via make install"
    if [[ "${SKIP_BUILD}" -eq 1 ]] && [[ -x "${DENIA_BIN}" ]]; then
        ok "skipped (--skip-build with existing ${DENIA_BIN})"
        return 0
    fi
    if [[ "${SKIP_BUILD}" -eq 1 ]] && [[ -x ./target/release/denia ]]; then
        run_cmd install -Dm0755 ./target/release/denia "${DENIA_BIN}"
        ok "installed binary from existing target/release/denia"
        return 0
    fi
    run_cmd make install
    ok "make install completed"
}

# ----------------------------------------------------------------------------
# Next-step hint
# ----------------------------------------------------------------------------

print_next_step() {
    cat <<EOF

  Denia binary installed at ${DENIA_BIN}.

  Provisioning is a separate step — paths, keys, config, and the systemd unit
  are created by the binary itself so they always track the daemon version.

  Next step:

    sudo denia setup

  Once setup completes, see "denia --help" for status/doctor/rotate-token/uninstall.

  To upgrade an existing install to the latest signed release:

    sudo denia update
EOF
}

# ----------------------------------------------------------------------------
# Entrypoint
# ----------------------------------------------------------------------------

main() {
    step_preflight_os
    step_preflight_kernel
    step_preflight_ports
    step_install_prereqs
    step_install_sops
    step_install_buildkit
    step_install_rust
    step_install_node
    step_make_install
    print_next_step
}

main "$@"

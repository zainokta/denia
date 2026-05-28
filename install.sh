#!/usr/bin/env bash
# Denia installer for a fresh single-node Linux host.
#
# Installs build prerequisites, builds the release binary + embedded SPA, lays
# out /var/lib/denia, creates the `denia` system user, writes a hardened
# systemd unit, and brings the service up on :80 / :443 / :7180.
#
# Config tunables surfaced in ~/.config/denia/config.toml mirror src/config.rs
# defaults. Do not invent fields; if you add a new tunable to FileConfig in
# config.rs, add it to write_config_file() too.

set -euo pipefail
IFS=$'\n\t'

# ----------------------------------------------------------------------------
# Constants / paths
# ----------------------------------------------------------------------------

readonly DENIA_USER="denia"
readonly DENIA_GROUP="denia"
readonly DENIA_HOME="/var/lib/denia"
readonly DENIA_BIN="/usr/local/bin/denia"
readonly DENIA_SYSTEMD_UNIT="/etc/systemd/system/denia.service"
readonly DENIA_BUILD_HOME="/usr/local/src/denia-build"

# Resolved at runtime by detect_install_user() from ${SUDO_USER}. The operator's
# config + admin token + age key live under their $HOME/.config/denia so they
# can edit without sudo. The daemon (running as the `denia` system user) reads
# them through a systemd BindReadOnlyPaths= bind mount, with files chmod'd to
# 0640 ${SUDO_USER}:denia so the daemon's group has read access.
DENIA_INSTALL_USER=""
DENIA_INSTALL_HOME=""
DENIA_USER_CONFIG_DIR=""
DENIA_CONFIG_FILE=""
DENIA_TOKEN_FILE=""
DENIA_AGE_KEY_FILE=""

# rustup-init official URL. Trust boundary: it is the upstream Rust project's
# canonical bootstrap; we still verify against an embedded SHA256 when one is
# pinned via DENIA_RUSTUP_SHA256, otherwise we log that the check was skipped
# (see step_install_rust).
readonly RUSTUP_INIT_URL="https://sh.rustup.rs"

# Node 22 LTS; pnpm is bootstrapped via corepack which ships with Node >= 16.
readonly NODE_MAJOR="22"

# ----------------------------------------------------------------------------
# CLI flags
# ----------------------------------------------------------------------------

DRY_RUN=0
UNINSTALL=0
PURGE=0
SKIP_BUILD=0

usage() {
    cat <<'EOF'
Usage: install.sh [--dry-run] [--skip-build] [--uninstall [--purge]]

Options:
  --dry-run      Print every command that would run, change nothing.
  --skip-build   Don't run cargo/pnpm. Expects ./target/release/denia present.
  --uninstall    Stop+disable the service, remove the binary and systemd unit.
                 Leaves /var/lib/denia and ~/.config/denia (operator config +
                 admin token + age key) alone.
  --purge        With --uninstall, also wipe /var/lib/denia and the operator's
                 ~/.config/denia directory.
  -h, --help     Show this help.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run)     DRY_RUN=1 ;;
        --uninstall)   UNINSTALL=1 ;;
        --purge)       PURGE=1 ;;
        --skip-build)  SKIP_BUILD=1 ;;
        -h|--help)     usage; exit 0 ;;
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

    # Reject non-glibc (musl) hosts: the rust release build links against the
    # host libc; mixing with musl Alpine needs a different toolchain.
    if [[ -x /usr/bin/ldd ]]; then
        if ! /usr/bin/ldd --version 2>&1 | /bin/grep -qiE "glibc|gnu libc"; then
            fail "Non-glibc libc detected (likely musl). This installer targets glibc distros."
        fi
    fi
    ok "Linux glibc host confirmed"

    if [[ "${EUID}" -ne 0 ]]; then
        fail "Must run as root (need /usr/local/bin, systemd, :80/:443). Try: sudo $0"
    fi
    ok "running as root"
}

detect_install_user() {
    step "Detect installing operator account"

    if [[ -z "${SUDO_USER:-}" ]] || [[ "${SUDO_USER:-}" == "root" ]]; then
        fail "install.sh must be invoked via sudo from a non-root account.
  Denia's config lives under that user's \$HOME/.config/denia/ so they can
  edit without sudo. Re-run as a regular user with: sudo ./install.sh"
    fi

    if ! /usr/bin/getent passwd "${SUDO_USER}" >/dev/null; then
        fail "SUDO_USER='${SUDO_USER}' has no /etc/passwd entry."
    fi

    DENIA_INSTALL_USER="${SUDO_USER}"
    DENIA_INSTALL_HOME="$(/usr/bin/getent passwd "${SUDO_USER}" | /usr/bin/cut -d: -f6)"
    if [[ -z "${DENIA_INSTALL_HOME}" ]] || [[ ! -d "${DENIA_INSTALL_HOME}" ]]; then
        fail "Cannot resolve a valid HOME for ${SUDO_USER} (got '${DENIA_INSTALL_HOME}')."
    fi

    DENIA_USER_CONFIG_DIR="${DENIA_INSTALL_HOME}/.config/denia"
    DENIA_CONFIG_FILE="${DENIA_USER_CONFIG_DIR}/config.toml"
    DENIA_TOKEN_FILE="${DENIA_USER_CONFIG_DIR}/admin.token"
    DENIA_AGE_KEY_FILE="${DENIA_USER_CONFIG_DIR}/age.key"

    ok "installer: ${DENIA_INSTALL_USER} (home: ${DENIA_INSTALL_HOME})"
    ok "config dir: ${DENIA_USER_CONFIG_DIR} (0750 ${DENIA_INSTALL_USER}:${DENIA_GROUP})"
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
        fail "Something is already listening on :80 or :443.
  Denia owns these ports (Pingora; see docs/adr/020-pingora-ingress.md).
  Remediation: stop and disable the offending service (Traefik, nginx, Caddy,
  Apache, ...) before re-running this installer."
    fi
    ok ":80 and :443 are free"
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
                build-essential pkg-config libssl-dev ca-certificates \
                curl git age iproute2 procps
            ;;
        dnf)
            run_cmd dnf install -y \
                @development-tools pkgconf-pkg-config openssl-devel ca-certificates \
                curl git age iproute procps-ng
            ;;
        pacman)
            run_cmd pacman -Sy --noconfirm \
                base-devel pkgconf openssl ca-certificates \
                curl git age iproute2 procps-ng
            ;;
        zypper)
            run_cmd zypper --non-interactive refresh
            run_cmd zypper --non-interactive install -y \
                -t pattern devel_basis
            run_cmd zypper --non-interactive install -y \
                pkg-config libopenssl-devel ca-certificates \
                curl git age iproute2 procps
            ;;
    esac
    ok "system packages installed"
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

    if [[ -x "${CARGO_HOME}/bin/cargo" ]]; then
        ok "rust toolchain already present at ${CARGO_HOME}"
        return 0
    fi

    # rustup-init: official upstream Rust bootstrap. We do not pipe an
    # unverified script blindly: we fetch to a tempfile, optionally verify
    # against DENIA_RUSTUP_SHA256, then execute with explicit flags. No
    # `curl | sh`.
    local tmp
    tmp="$(/usr/bin/mktemp)"
    # shellcheck disable=SC2064 # we want $tmp expanded now, this trap is local
    trap "rm -f '${tmp}'" RETURN

    run_cmd /usr/bin/curl --proto '=https' --tlsv1.2 -sSfL "${RUSTUP_INIT_URL}" -o "${tmp}"

    if [[ -n "${DENIA_RUSTUP_SHA256:-}" ]]; then
        local actual
        actual="$(/usr/bin/sha256sum "${tmp}" | /usr/bin/awk '{print $1}')"
        if [[ "${actual}" != "${DENIA_RUSTUP_SHA256}" ]]; then
            fail "rustup-init sha256 mismatch: got ${actual}, expected ${DENIA_RUSTUP_SHA256}"
        fi
        ok "rustup-init sha256 verified"
    else
        warn "DENIA_RUSTUP_SHA256 not set; skipping rustup-init checksum verification.
  Set DENIA_RUSTUP_SHA256=<known-good> for a fully pinned install."
    fi

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
# Node 22 + pnpm + web build
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
                # but only run after a sanity check; trust boundary documented.
                run_sh "/usr/bin/curl --proto '=https' --tlsv1.2 -fsSL https://deb.nodesource.com/setup_${NODE_MAJOR}.x -o /tmp/nodesource-setup.sh"
                run_cmd /bin/bash /tmp/nodesource-setup.sh
                run_cmd env DEBIAN_FRONTEND=noninteractive apt-get install -y nodejs
                run_cmd rm -f /tmp/nodesource-setup.sh
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
        run_cmd corepack prepare pnpm@latest --activate
        ok "pnpm via corepack"
    else
        run_cmd npm install -g pnpm
        ok "pnpm via npm -g (corepack unavailable)"
    fi
}

step_build_web() {
    step "Build web SPA (pnpm install + pnpm build)"
    if [[ "${SKIP_BUILD}" -eq 1 ]]; then
        ok "skipped (--skip-build)"
        return 0
    fi

    # Idempotent: re-running is safe because pnpm/install + vite build are
    # both idempotent. We do not delete dist; vite rewrites it.
    (
        cd ./web
        run_cmd pnpm install --frozen-lockfile
        run_cmd pnpm build
    )

    if [[ "${DRY_RUN}" -eq 0 ]] && [[ ! -d ./web/dist/client ]]; then
        fail "web/dist/client missing after pnpm build; cargo release build will fail"
    fi
    ok "web/dist/client ready"
}

# ----------------------------------------------------------------------------
# Rust release build
# ----------------------------------------------------------------------------

step_build_rust() {
    step "Build Rust release binary"
    if [[ "${SKIP_BUILD}" -eq 1 ]]; then
        if [[ ! -x ./target/release/denia ]]; then
            fail "--skip-build set but ./target/release/denia is missing"
        fi
        ok "skipped (--skip-build); existing binary at ./target/release/denia"
        return 0
    fi

    export CARGO_HOME="${DENIA_BUILD_HOME}/cargo"
    export RUSTUP_HOME="${DENIA_BUILD_HOME}/rustup"
    export PATH="${CARGO_HOME}/bin:${PATH}"

    run_cmd cargo build --release --locked

    if [[ "${DRY_RUN}" -eq 0 ]] && [[ ! -x ./target/release/denia ]]; then
        fail "./target/release/denia not produced by cargo"
    fi
    ok "cargo build --release OK"
}

# ----------------------------------------------------------------------------
# System user + on-disk layout
# ----------------------------------------------------------------------------

step_create_user() {
    step "Create '${DENIA_USER}' system user"

    if /usr/bin/getent group "${DENIA_GROUP}" >/dev/null; then
        ok "group ${DENIA_GROUP} exists"
    else
        run_cmd /usr/sbin/groupadd --system "${DENIA_GROUP}"
        ok "group ${DENIA_GROUP} created"
    fi

    if /usr/bin/getent passwd "${DENIA_USER}" >/dev/null; then
        ok "user ${DENIA_USER} exists"
    else
        run_cmd /usr/sbin/useradd \
            --system \
            --gid "${DENIA_GROUP}" \
            --home-dir "${DENIA_HOME}" \
            --no-create-home \
            --shell /usr/sbin/nologin \
            "${DENIA_USER}"
        ok "user ${DENIA_USER} created"
    fi
}

step_create_layout() {
    step "Create on-disk layout under ${DENIA_HOME}"

    # Mirror src/config.rs:
    #   data_dir          = /var/lib/denia                (DENIA_DATA_DIR)
    #   database_path     = data_dir / "denia.sqlite3"    (DENIA_DATABASE_PATH)
    #   runtime_dir       = data_dir / "runtime"
    #   artifact_dir      = data_dir / "artifacts"
    #   log_dir           = data_dir / "logs"
    #   tls_dir           = data_dir / "tls"              (DENIA_TLS_DIR)
    #   cgroup_root       = /sys/fs/cgroup/denia          (DENIA_CGROUP_ROOT)
    # Plus operator-defined dirs not in config.rs but needed by the project:
    #   sqlite/, secrets/  (sqlite is for the .sqlite3 file's parent; secrets/
    #                       holds the age key referenced by SOPS)
    local dirs=(
        "${DENIA_HOME}"
        "${DENIA_HOME}/sqlite"
        "${DENIA_HOME}/artifacts"
        "${DENIA_HOME}/tls"
        "${DENIA_HOME}/secrets"
        "${DENIA_HOME}/runtime"
        "${DENIA_HOME}/logs"
    )
    local d
    for d in "${dirs[@]}"; do
        run_cmd install -d -m 0700 -o "${DENIA_USER}" -g "${DENIA_GROUP}" "${d}"
    done
    ok "layout ready (0700, ${DENIA_USER}:${DENIA_GROUP})"

    # cgroup root under /sys/fs/cgroup/denia. Created here so the daemon never
    # needs to mkdir() at the root unprivileged. Owned by denia:denia + a
    # delegation file so systemd's cgroup v2 delegation is explicit.
    run_cmd install -d -m 0755 -o "${DENIA_USER}" -g "${DENIA_GROUP}" /sys/fs/cgroup/denia
    ok "cgroup root /sys/fs/cgroup/denia delegated"

    # Operator config dir under their $HOME. Owned by the installer + the denia
    # group so the human edits without sudo and the daemon (in `denia` group)
    # reads via group bits. Parent ~/.config gets created with installer-only
    # perms; the denia subdir is 0750 installer:denia.
    run_cmd install -d -m 0700 -o "${DENIA_INSTALL_USER}" -g "${DENIA_INSTALL_USER}" \
        "${DENIA_INSTALL_HOME}/.config"
    run_cmd install -d -m 0750 -o "${DENIA_INSTALL_USER}" -g "${DENIA_GROUP}" \
        "${DENIA_USER_CONFIG_DIR}"
    ok "user config dir ${DENIA_USER_CONFIG_DIR} ready"
}

# ----------------------------------------------------------------------------
# Secrets: age key + admin bootstrap token
# ----------------------------------------------------------------------------

step_age_key() {
    step "Generate age identity for SOPS (if absent)"
    local key="${DENIA_AGE_KEY_FILE}"

    if [[ -s "${key}" ]]; then
        ok "age key already present at ${key} (keeping)"
        return 0
    fi
    if ! command -v age-keygen >/dev/null 2>&1; then
        fail "age-keygen missing; package install step should have provided it"
    fi
    # age-keygen writes to stdout; capture without leaking through shell history.
    if [[ "${DRY_RUN}" -eq 1 ]]; then
        printf '  %s[dry-run]%s age-keygen -o %s\n' "${C_YELLOW}" "${C_RESET}" "${key}"
    else
        # `install` then write: create an empty 0640 file owned by the operator
        # with denia group read; then have age-keygen write the body. Avoids a
        # window where the key sits world-readable.
        install -m 0640 -o "${DENIA_INSTALL_USER}" -g "${DENIA_GROUP}" /dev/null "${key}"
        # age-keygen writes both the private key and a "# public key:" comment
        # to the file; the whole thing is what we keep.
        age-keygen -o "${key}" >/dev/null 2>&1
        chmod 0640 "${key}"
        chown "${DENIA_INSTALL_USER}:${DENIA_GROUP}" "${key}"
    fi
    ok "age key written to ${key} (0640 ${DENIA_INSTALL_USER}:${DENIA_GROUP})"
}

step_admin_token() {
    step "Generate admin bootstrap token (if absent)"
    if [[ -s "${DENIA_TOKEN_FILE}" ]]; then
        ok "admin token already present at ${DENIA_TOKEN_FILE} (keeping)"
        return 0
    fi
    if [[ "${DRY_RUN}" -eq 1 ]]; then
        printf '  %s[dry-run]%s generate 32 bytes hex -> %s\n' "${C_YELLOW}" "${C_RESET}" "${DENIA_TOKEN_FILE}"
        return 0
    fi

    # 32 bytes -> 64 hex chars, meeting AppConfig::from_env's >=64 floor.
    local hex
    hex="$(/usr/bin/head -c 32 /dev/urandom | /usr/bin/od -An -vtx1 | /usr/bin/tr -d ' \n')"
    install -m 0640 -o "${DENIA_INSTALL_USER}" -g "${DENIA_GROUP}" /dev/null "${DENIA_TOKEN_FILE}"
    # Systemd EnvironmentFile= parses KEY=VALUE lines, so write it in that form.
    /usr/bin/printf 'DENIA_ADMIN_TOKEN=%s\n' "${hex}" > "${DENIA_TOKEN_FILE}"
    chmod 0640 "${DENIA_TOKEN_FILE}"
    chown "${DENIA_INSTALL_USER}:${DENIA_GROUP}" "${DENIA_TOKEN_FILE}"
    ok "admin token written to ${DENIA_TOKEN_FILE} (0640 ${DENIA_INSTALL_USER}:${DENIA_GROUP})"
}

# ----------------------------------------------------------------------------
# Install binary
# ----------------------------------------------------------------------------

step_install_binary() {
    step "Install binary to ${DENIA_BIN}"
    if [[ ! -x ./target/release/denia ]] && [[ "${DRY_RUN}" -eq 0 ]]; then
        fail "./target/release/denia not found"
    fi
    run_cmd install -m 0755 -o root -g root ./target/release/denia "${DENIA_BIN}"
    ok "${DENIA_BIN} installed"

    # SPA bundle is baked into the binary via rust-embed (src/web.rs +
    # web/dist/client). No separate /var/lib/denia/web/ copy needed.
}

# ----------------------------------------------------------------------------
# Config file (TOML)
# ----------------------------------------------------------------------------

write_config_file() {
    # TOML config consumed by src/config.rs::FileConfig. The systemd unit pins
    # DENIA_CONFIG_FILE to this path and BindReadOnlyPaths= bind-mounts the
    # whole config dir into the daemon's view. The operator owns the file
    # (denia group has read), so editing requires no sudo. The admin token
    # lives in admin.token in the same dir, loaded as EnvironmentFile so the
    # daemon never has to read it via the TOML.
    cat > "${DENIA_CONFIG_FILE}" <<EOF
# ${DENIA_CONFIG_FILE}
# Denia control-plane configuration (TOML; mirrors src/config.rs::FileConfig).
# Read by the daemon at boot. DENIA_* environment variables in the systemd unit
# override per-field (see docs/adr/023). The admin token lives in
# ${DENIA_TOKEN_FILE}; keep it out of this file.

# --- Control plane bind ---
# Management API socket. Pingora handles :80/:443; this is only the
# operator-facing /v1 + /healthz + SPA.
bind_addr = "0.0.0.0:7180"

# --- On-disk layout ---
data_dir = "${DENIA_HOME}"
database_path = "${DENIA_HOME}/sqlite/denia.sqlite3"
tls_dir = "${DENIA_HOME}/tls"
node_disk_path = "${DENIA_HOME}"

# --- Workload runtime ---
cgroup_root = "/sys/fs/cgroup/denia"
# Subuid/subgid mapping for workloads (see ADR-005). Default 100000 / 65536;
# raise userns_size if you run many workloads.
userns_base = 100000
userns_size = 65536

# --- External tool paths (PATH lookup if commented) ---
# buildkit_binary = "/usr/local/bin/buildctl"
# git_binary = "/usr/bin/git"
# sops_binary = "/usr/local/bin/sops"

# --- Ingress (Pingora) ---
# ACME HTTP-01 lives on :80; keep it 80 unless you front it (don't -- Denia
# owns these).
http_port = 80
https_port = 443
# Let's Encrypt production. Use the staging URL on non-prod nodes:
# acme_directory_url = "https://acme-staging-v02.api.letsencrypt.org/directory"
acme_directory_url = "https://acme-v02.api.letsencrypt.org/directory"
# Required if any service has tls_enabled = true:
# acme_email = "ops@example.com"

# --- Control-plane TLS gating ---
# control_domain = "denia.example.com"
# control_tls = false

# --- Autoscaler ---
autoscale_interval_s = 15
autoscale_headroom_cpu_millis = 1000
autoscale_headroom_mem_bytes = 536870912

# --- Control-plane secret encryption (ADR-021) ---
# Age private key file. The public recipient is auto-derived from the
# "# public key:" comment unless age_recipient is set explicitly.
age_key_file = "${DENIA_AGE_KEY_FILE}"
EOF
    chmod 0640 "${DENIA_CONFIG_FILE}"
    chown "${DENIA_INSTALL_USER}:${DENIA_GROUP}" "${DENIA_CONFIG_FILE}"
}

step_config_file() {
    step "Write ${DENIA_CONFIG_FILE}"
    if [[ -f "${DENIA_CONFIG_FILE}" ]]; then
        ok "config file already present (keeping; edit by hand to change defaults)"
        return 0
    fi
    if [[ "${DRY_RUN}" -eq 1 ]]; then
        printf '  %s[dry-run]%s write %s (0640 %s:%s)\n' \
            "${C_YELLOW}" "${C_RESET}" "${DENIA_CONFIG_FILE}" \
            "${DENIA_INSTALL_USER}" "${DENIA_GROUP}"
        return 0
    fi
    write_config_file
    ok "${DENIA_CONFIG_FILE} written"
}

# ----------------------------------------------------------------------------
# systemd unit
# ----------------------------------------------------------------------------

write_systemd_unit() {
    cat > "${DENIA_SYSTEMD_UNIT}" <<EOF
[Unit]
Description=Denia control plane + L7 ingress (Pingora)
Documentation=file:${PWD}/docs/adr/020-pingora-ingress.md
After=network-online.target
Wants=network-online.target
# Denia owns :80/:443; refuse to coexist with the typical reverse proxies.
Conflicts=traefik.service nginx.service caddy.service apache2.service httpd.service

[Service]
Type=simple
User=${DENIA_USER}
Group=${DENIA_GROUP}
WorkingDirectory=${DENIA_HOME}

# Config file is the source of truth (TOML); admin token is supplied via env
# from a separate file for credential hygiene. Env wins per-field, so the
# token EnvironmentFile is loaded after the config-file pin.
# SOPS_AGE_KEY_FILE is read by the external sops binary the daemon shells out
# to at deploy time (see ADR-021); pinning it here keeps that path consistent
# with config.toml's age_key_file.
Environment=DENIA_CONFIG_FILE=${DENIA_CONFIG_FILE}
Environment=SOPS_AGE_KEY_FILE=${DENIA_AGE_KEY_FILE}
EnvironmentFile=${DENIA_TOKEN_FILE}

ExecStart=${DENIA_BIN}
Restart=on-failure
RestartSec=3s
LimitNOFILE=1048576
TimeoutStopSec=30s

# --- Capabilities ---
# Denia's workload launcher creates user/mount/pid/uts/ipc namespaces and
# writes cgroup v2 controllers -> CAP_SYS_ADMIN. Pingora binds :80/:443 as a
# non-root user -> CAP_NET_BIND_SERVICE. CAP_SETUID/SETGID are needed to
# write the child uid_map/gid_map and to drop into the mapped user (see
# src/syscall/ns.rs).
AmbientCapabilities=CAP_NET_BIND_SERVICE CAP_SYS_ADMIN CAP_SETUID CAP_SETGID
CapabilityBoundingSet=CAP_NET_BIND_SERVICE CAP_SYS_ADMIN CAP_SETUID CAP_SETGID

# --- Filesystem confinement ---
# Workload launcher and rustix syscall helpers handle the child's
# no_new_privs + bounding-set drop (src/syscall/caps.rs); we do NOT set
# NoNewPrivileges=true here because that would block the parent from
# applying it post-fork.
NoNewPrivileges=false
ProtectSystem=strict
# ProtectHome=true would hide /home entirely; we keep it true and punch a
# narrow read-only bind mount through to the operator's denia config dir so
# the daemon can read config.toml + admin.token + age.key without seeing the
# rest of the user's home.
ProtectHome=true
BindReadOnlyPaths=${DENIA_USER_CONFIG_DIR}
PrivateTmp=true
ProtectKernelLogs=true
ProtectKernelModules=true
ProtectClock=true
ProtectControlGroups=false
ReadWritePaths=${DENIA_HOME} /sys/fs/cgroup
# Denia must be able to mount/unmount inside child namespaces.
MountFlags=shared

# --- Cgroup v2 delegation ---
Delegate=yes
DelegateControllers=cpu cpuset io memory pids

[Install]
WantedBy=multi-user.target
EOF
    chmod 0644 "${DENIA_SYSTEMD_UNIT}"
}

step_systemd_unit() {
    step "Write systemd unit ${DENIA_SYSTEMD_UNIT}"
    if [[ "${DRY_RUN}" -eq 1 ]]; then
        printf '  %s[dry-run]%s write %s\n' "${C_YELLOW}" "${C_RESET}" "${DENIA_SYSTEMD_UNIT}"
        return 0
    fi
    write_systemd_unit
    ok "systemd unit written"
}

# ----------------------------------------------------------------------------
# Enable + start
# ----------------------------------------------------------------------------

step_start_service() {
    step "Enable + start denia.service"
    run_cmd systemctl daemon-reload
    run_cmd systemctl enable --now denia.service

    if [[ "${DRY_RUN}" -eq 1 ]]; then
        return 0
    fi

    # Brief wait for the service to settle; fail loud if it didn't.
    local i=0
    while [[ "${i}" -lt 10 ]]; do
        if systemctl is-active --quiet denia.service; then
            ok "denia.service is active"
            return 0
        fi
        i=$((i + 1))
        sleep 1
    done

    /bin/echo "denia.service did not become active. Last 50 log lines:" >&2
    journalctl -u denia.service -n 50 --no-pager >&2 || true
    fail "denia.service is not active"
}

# ----------------------------------------------------------------------------
# Final summary
# ----------------------------------------------------------------------------

step_summary() {
    step "Done"

    local host
    host="$(/bin/hostname -f 2>/dev/null || /bin/hostname)"
    cat <<EOF

  Denia is installed and running.

  Control plane:     http://${host}:7180/  (also http://127.0.0.1:7180/)
  Web console:       http://${host}:7180/  (SPA served by the binary)
  Public ingress:    :80 / :443 (Pingora; ACME via Let's Encrypt production)

  Files:
    binary:          ${DENIA_BIN}
    systemd unit:    ${DENIA_SYSTEMD_UNIT}
    config:          ${DENIA_CONFIG_FILE}        (0640 ${DENIA_INSTALL_USER}:${DENIA_GROUP})
    admin token:     ${DENIA_TOKEN_FILE}         (0640 ${DENIA_INSTALL_USER}:${DENIA_GROUP})
    age identity:    ${DENIA_AGE_KEY_FILE}       (0640 ${DENIA_INSTALL_USER}:${DENIA_GROUP})
    data root:       ${DENIA_HOME}               (denia:denia)
    cgroup root:     /sys/fs/cgroup/denia        (denia:denia)

  Edit config without sudo: \$EDITOR ${DENIA_CONFIG_FILE}
  Restart after edits:      sudo systemctl restart denia

  Next step -- bootstrap the first admin account (one-time):

    TOKEN="\$(sed -n 's/^DENIA_ADMIN_TOKEN=//p' ${DENIA_TOKEN_FILE})"
    curl -fsS -X POST \\
      -H "Authorization: Bearer \$TOKEN" \\
      -H 'Content-Type: application/json' \\
      -d '{"username":"admin","password":"<choose-a-strong-password>"}' \\
      http://${host}:7180/v1/bootstrap

  Or open the /setup page in the web console and paste the token there.

  Operate:
    journalctl -u denia -f
    systemctl status denia
    systemctl restart denia

EOF
}

# ----------------------------------------------------------------------------
# Uninstall
# ----------------------------------------------------------------------------

do_uninstall() {
    step "Uninstall denia"
    if command -v systemctl >/dev/null 2>&1; then
        run_cmd systemctl disable --now denia.service || true
    fi
    run_cmd rm -f "${DENIA_SYSTEMD_UNIT}"
    if command -v systemctl >/dev/null 2>&1; then
        run_cmd systemctl daemon-reload || true
    fi
    run_cmd rm -f "${DENIA_BIN}"

    # Operator-owned config (~/.config/denia) is intentionally left in place;
    # it contains the admin token and age key the user may want to back up or
    # reuse on reinstall. --purge removes it below.

    if [[ "${PURGE}" -eq 1 ]]; then
        step "--purge: wiping ${DENIA_HOME}, /sys/fs/cgroup/denia, and ${DENIA_USER_CONFIG_DIR}"
        run_cmd rm -rf "${DENIA_HOME}"
        run_cmd rmdir /sys/fs/cgroup/denia 2>/dev/null || true
        if [[ -n "${DENIA_USER_CONFIG_DIR}" ]] && [[ -d "${DENIA_USER_CONFIG_DIR}" ]]; then
            run_cmd rm -rf "${DENIA_USER_CONFIG_DIR}"
        fi
        if /usr/bin/getent passwd "${DENIA_USER}" >/dev/null; then
            run_cmd /usr/sbin/userdel "${DENIA_USER}" || true
        fi
        if /usr/bin/getent group "${DENIA_GROUP}" >/dev/null; then
            run_cmd /usr/sbin/groupdel "${DENIA_GROUP}" || true
        fi
        ok "purge complete"
    else
        ok "data (${DENIA_HOME}), config (${DENIA_USER_CONFIG_DIR}), and user '${DENIA_USER}' preserved. Use --purge to remove."
    fi
}

# ----------------------------------------------------------------------------
# Entrypoint
# ----------------------------------------------------------------------------

main() {
    if [[ "${UNINSTALL}" -eq 1 ]]; then
        if [[ "${EUID}" -ne 0 ]]; then
            fail "uninstall requires root"
        fi
        # Resolve the operator's config-dir path so --purge can wipe it; the
        # detector also enforces SUDO_USER, matching the install entry path.
        detect_install_user
        do_uninstall
        exit 0
    fi

    step_preflight_os
    detect_install_user
    step_preflight_kernel
    step_preflight_ports
    step_install_prereqs
    step_install_rust
    step_install_node
    step_build_web
    step_build_rust
    step_create_user
    step_create_layout
    step_age_key
    step_admin_token
    step_install_binary
    step_config_file
    step_systemd_unit
    step_start_service
    step_summary
}

main "$@"

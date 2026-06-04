use denia::{
    artifacts::{ArtifactKind, ArtifactRecord, ArtifactSource},
    domain::{RuntimeInstanceId, RuntimeStartRequest},
    runtime::{LinuxRuntime, LinuxRuntimeProcessSpec, Runtime, RuntimeConsoleRequest},
    syscall::{
        self,
        ns::{NamespaceConfig, OverlaySpec, RoBind, spawn_namespaced_process},
    },
};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};

fn static_busybox() -> PathBuf {
    std::env::var_os("DENIA_PRIVILEGED_BUSYBOX_STATIC")
        .map(PathBuf::from)
        .or_else(|| {
            ["/usr/lib/nix/busybox"]
                .into_iter()
                .map(PathBuf::from)
                .find(|path| path.exists())
        })
        .expect("DENIA_PRIVILEGED_BUSYBOX_STATIC must point to a static busybox binary")
}

fn socket_proxy_helper(build_dir: &Path) -> PathBuf {
    if let Some(path) = std::env::var_os("DENIA_PRIVILEGED_DENIA_HELPER_STATIC").map(PathBuf::from)
    {
        return path;
    }

    let source = build_dir.join("denia-test-socket-proxy.c");
    let binary = build_dir.join("denia-test-socket-proxy");
    std::fs::write(&source, TEST_SOCKET_PROXY_C).expect("write test socket proxy source");
    let status = std::process::Command::new("cc")
        .args(["-static", "-O2", "-o"])
        .arg(&binary)
        .arg(&source)
        .status()
        .expect("run cc for static test socket proxy");
    assert!(
        status.success(),
        "cc -static must build the fallback socket-proxy test helper, got {status}"
    );
    binary
}

fn write_busybox_rootfs(rootfs: &Path) {
    let bin_dir = rootfs.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    let busybox = bin_dir.join("busybox");
    std::fs::copy(static_busybox(), &busybox).expect("copy busybox");
    let mut permissions = std::fs::metadata(&busybox)
        .expect("busybox metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&busybox, permissions).expect("busybox permissions");
    symlink("busybox", bin_dir.join("sleep")).expect("sleep symlink");
    symlink("busybox", bin_dir.join("sh")).expect("sh symlink");
    symlink("busybox", bin_dir.join("cat")).expect("cat symlink");
    std::fs::create_dir_all(rootfs.join("proc")).expect("proc dir");
    std::fs::create_dir_all(rootfs.join("tmp")).expect("tmp dir");
}

async fn read_pty_output(pty: &mut (dyn tokio::io::AsyncRead + Unpin + Send)) -> Vec<u8> {
    use tokio::io::AsyncReadExt as _;

    let mut output = Vec::new();
    let mut buf = [0_u8; 1024];
    loop {
        match pty.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => output.extend_from_slice(&buf[..n]),
            Err(error) if error.raw_os_error() == Some(5) => break,
            Err(error) => panic!("read console output: {error}"),
        }
    }
    output
}

struct CgroupTestRoot {
    path: PathBuf,
}

impl CgroupTestRoot {
    fn new() -> Self {
        let parent = std::env::var_os("DENIA_PRIVILEGED_CGROUP_PARENT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/sys/fs/cgroup"));
        assert!(
            parent.join("cgroup.controllers").exists(),
            "{} must be a cgroup v2 directory; set DENIA_PRIVILEGED_CGROUP_PARENT to a writable cgroup v2 parent",
            parent.display()
        );
        enable_cgroup_controllers(&parent).unwrap_or_else(|error| {
            panic!(
                "enable cpu/memory controllers under {}: {error}; set DENIA_PRIVILEGED_CGROUP_PARENT to a delegated empty cgroup v2 parent",
                parent.display()
            )
        });

        let path = parent.join(format!(
            "denia-test-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        fs::create_dir(&path).unwrap_or_else(|error| {
            panic!(
                "create privileged cgroup test root {}: {error}; run as root with a writable cgroup v2 parent",
                path.display()
            )
        });
        enable_cgroup_controllers(&path).unwrap_or_else(|error| {
            panic!(
                "enable cpu/memory controllers under {}: {error}; set DENIA_PRIVILEGED_CGROUP_PARENT to a delegated empty cgroup v2 parent",
                path.display()
            )
        });
        let root = Self { path };
        root.assert_writable_leaf("probe");
        root
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn create_leaf(&self, name: &str) -> PathBuf {
        let path = self.path.join(format!("{name}-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&path).unwrap_or_else(|error| {
            panic!(
                "create privileged cgroup leaf {}: {error}; set DENIA_PRIVILEGED_CGROUP_PARENT to a delegated empty cgroup v2 parent",
                path.display()
            )
        });
        assert_cgroup_limit_writable(&path, "cpu.max", "10000 100000\n");
        assert_cgroup_limit_writable(&path, "memory.max", "67108864\n");
        path
    }

    fn assert_writable_leaf(&self, name: &str) {
        let path = self.create_leaf(name);
        let mut child = std::process::Command::new("sleep")
            .arg("1")
            .spawn()
            .expect("spawn cgroup probe child");
        fs::write(path.join("cgroup.procs"), format!("{}\n", child.id())).unwrap_or_else(|error| {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "attach probe pid to {}: {error}; set DENIA_PRIVILEGED_CGROUP_PARENT to a delegated empty cgroup v2 parent with cpu and memory controllers",
                path.display()
            )
        });
        let _ = child.kill();
        let _ = child.wait();
        remove_cgroup_dir(&path).unwrap_or_else(|error| {
            panic!("remove privileged cgroup probe {}: {error}", path.display())
        });
    }
}

impl Drop for CgroupTestRoot {
    fn drop(&mut self) {
        let _ = fs::write(self.path.join("cgroup.kill"), "1\n");
        let _ = remove_cgroup_dir(&self.path);
    }
}

fn remove_cgroup_dir(path: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            remove_cgroup_dir(&entry.path())?;
        }
    }
    fs::remove_dir(path)
}

fn enable_cgroup_controllers(path: &Path) -> std::io::Result<()> {
    let controllers_path = path.join("cgroup.controllers");
    let subtree_control_path = path.join("cgroup.subtree_control");
    if !controllers_path.exists() || !subtree_control_path.exists() {
        return Ok(());
    }

    let available = fs::read_to_string(&controllers_path)?;
    let requested = ["cpu", "memory"]
        .into_iter()
        .filter(|controller| {
            available
                .split_whitespace()
                .any(|available| available == *controller)
        })
        .map(|controller| format!("+{controller}"))
        .collect::<Vec<_>>();
    if requested.is_empty() {
        return Ok(());
    }

    fs::write(subtree_control_path, format!("{}\n", requested.join(" ")))
}

fn assert_cgroup_limit_writable(path: &Path, file_name: &str, value: &str) {
    fs::write(path.join(file_name), value).unwrap_or_else(|error| {
        panic!(
            "write {} under {}: {error}; set DENIA_PRIVILEGED_CGROUP_PARENT to a delegated empty cgroup v2 parent with cpu and memory controllers",
            file_name,
            path.display()
        )
    });
}

fn wait_for_path(path: &Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if path.exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("timed out waiting for {}", path.display());
}

const TEST_SOCKET_PROXY_C: &str = r#"
#include <errno.h>
#include <signal.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

static void mkdirs(char *path) {
    for (char *p = path + 1; *p; p++) {
        if (*p == '/') {
            *p = '\0';
            if (mkdir(path, 0777) < 0 && errno != EEXIST) {
                perror("mkdir");
                exit(111);
            }
            *p = '/';
        }
    }
}

int main(int argc, char **argv) {
    const char *listen_path = NULL;
    int child_index = -1;

    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--listen") == 0 && i + 1 < argc) {
            listen_path = argv[++i];
            continue;
        }
        if (strcmp(argv[i], "--connect") == 0 && i + 1 < argc) {
            i++;
            continue;
        }
        if (strcmp(argv[i], "--") == 0) {
            child_index = i + 1;
            break;
        }
    }

    if (!listen_path || child_index < 0 || child_index >= argc) {
        fprintf(stderr, "invalid test socket proxy args\n");
        return 112;
    }

    char parent[108];
    if (strlen(listen_path) >= sizeof(parent)) {
        fprintf(stderr, "listen path too long\n");
        return 113;
    }
    strcpy(parent, listen_path);
    char *slash = strrchr(parent, '/');
    if (slash && slash != parent) {
        *slash = '\0';
        mkdirs(parent);
        if (mkdir(parent, 0777) < 0 && errno != EEXIST) {
            perror("mkdir parent");
            return 114;
        }
    }

    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0) {
        perror("socket");
        return 115;
    }

    struct sockaddr_un addr;
    memset(&addr, 0, sizeof(addr));
    addr.sun_family = AF_UNIX;
    strncpy(addr.sun_path, listen_path, sizeof(addr.sun_path) - 1);
    unlink(listen_path);
    if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        perror("bind");
        return 116;
    }
    if (listen(fd, 16) < 0) {
        perror("listen");
        return 117;
    }

    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return 118;
    }
    if (child == 0) {
        execv(argv[child_index], &argv[child_index]);
        perror("execv");
        _exit(119);
    }

    int status = 0;
    if (waitpid(child, &status, 0) < 0) {
        perror("waitpid");
        return 120;
    }
    if (WIFEXITED(status)) {
        return WEXITSTATUS(status);
    }
    if (WIFSIGNALED(status)) {
        return 128 + WTERMSIG(status);
    }
    return 121;
}
"#;

#[test]
#[ignore = "requires root, cgroup v2, and Linux namespace permissions"]
fn privileged_runtime_tests_are_explicitly_gated() {
    assert_eq!(
        std::env::var("DENIA_RUN_PRIVILEGED_TESTS").as_deref(),
        Ok("1")
    );
}

#[tokio::test]
#[ignore = "requires root, cgroup v2, Linux namespace permissions, and DENIA_PRIVILEGED_BUSYBOX_STATIC"]
async fn linux_runtime_start_uses_native_namespace_and_cgroup_gate() {
    assert_eq!(
        std::env::var("DENIA_RUN_PRIVILEGED_TESTS").as_deref(),
        Ok("1")
    );
    assert!(
        std::fs::read_to_string("/proc/self/status")
            .expect("status")
            .lines()
            .any(|line| line == "Uid:\t0\t0\t0\t0"),
        "privileged runtime tests must run as root"
    );
    assert!(
        static_busybox().exists(),
        "static busybox must exist through DENIA_PRIVILEGED_BUSYBOX_STATIC or /usr/lib/nix/busybox"
    );
    let runtime_dir = tempfile::tempdir().expect("runtime dir");
    let artifact_dir = tempfile::tempdir().expect("artifact dir");
    let helper_dir = tempfile::tempdir().expect("helper dir");
    let cgroup_root = CgroupTestRoot::new();
    let socket_proxy = socket_proxy_helper(helper_dir.path());
    assert!(
        socket_proxy.exists(),
        "socket proxy helper must exist at {}",
        socket_proxy.display()
    );
    let runtime =
        LinuxRuntime::new_with_paths(runtime_dir.path(), artifact_dir.path(), cgroup_root.path())
            .with_socket_proxy(socket_proxy);
    let artifact = ArtifactRecord::new(
        "sha256:true",
        ArtifactKind::RootfsBundle,
        ArtifactSource::ExternalRegistry {
            image: "local/rootfs:true".to_string(),
        },
    )
    .expect("artifact");
    let bundle_dir = artifact_dir.path().join("sha256-true");
    let rootfs = bundle_dir.join("rootfs");
    write_busybox_rootfs(&rootfs);
    std::fs::write(
        bundle_dir.join("process.json"),
        serde_json::to_vec(&LinuxRuntimeProcessSpec {
            argv: vec!["/bin/sleep".to_string(), "30".to_string()],
            env: Vec::new(),
            workdir: "/".to_string(),
        })
        .expect("manifest json"),
    )
    .expect("manifest");

    let deployment_id = uuid::Uuid::now_v7();
    let status = runtime
        .start(RuntimeStartRequest {
            service_name: "true-service".to_string(),
            service_id: uuid::Uuid::now_v7(),
            deployment_id,
            artifact,
            internal_port: 3000,
            socket_path: runtime_dir.path().join("true-service/current.sock"),
            cpu_millis: 100,
            memory_bytes: 67108864,
            env: Vec::new(),
            pids_max: None,
            memory_swap_max: None,
            io_weight: None,
            replica_index: 0,
        })
        .await
        .expect("runtime start");

    assert_eq!(status.state, "running");
    assert!(status.pid.is_some());
    assert!(status.cgroup_path.starts_with(cgroup_root.path()));
    assert_eq!(
        fs::read_to_string(status.cgroup_path.join("cpu.max")).expect("cpu.max"),
        "10000 100000\n"
    );
    assert_eq!(
        fs::read_to_string(status.cgroup_path.join("memory.max")).expect("memory.max"),
        "67108864\n"
    );
    wait_for_path(&status.socket_path);
    runtime
        .stop(&RuntimeInstanceId {
            service_id: status.service_id,
            service_name: "true-service".to_string(),
            replica_index: 0,
        })
        .await
        .expect("stop runtime process");
    assert!(
        !status.cgroup_path.exists(),
        "runtime stop should remove deployment cgroup {}",
        status.cgroup_path.display()
    );
}

#[tokio::test]
#[ignore = "requires root, cgroup v2, Linux namespace permissions, and DENIA_PRIVILEGED_BUSYBOX_STATIC"]
async fn sweep_orphans_reaps_workload_from_a_previous_session() {
    // Simulate an unclean prior session: start a real workload, then abandon the
    // in-memory tracking by building a FRESH LinuxRuntime over the same dirs
    // (as a daemon restart does). `list_running` is empty there, so only the
    // filesystem+cgroup sweep can reap the survivor.
    assert_eq!(
        std::env::var("DENIA_RUN_PRIVILEGED_TESTS").as_deref(),
        Ok("1")
    );
    assert!(
        std::fs::read_to_string("/proc/self/status")
            .expect("status")
            .lines()
            .any(|line| line == "Uid:\t0\t0\t0\t0"),
        "privileged runtime tests must run as root"
    );
    assert!(
        static_busybox().exists(),
        "static busybox must exist through DENIA_PRIVILEGED_BUSYBOX_STATIC or /usr/lib/nix/busybox"
    );
    let runtime_dir = tempfile::tempdir().expect("runtime dir");
    let artifact_dir = tempfile::tempdir().expect("artifact dir");
    let helper_dir = tempfile::tempdir().expect("helper dir");
    let cgroup_root = CgroupTestRoot::new();
    let socket_proxy = socket_proxy_helper(helper_dir.path());
    let runtime =
        LinuxRuntime::new_with_paths(runtime_dir.path(), artifact_dir.path(), cgroup_root.path())
            .with_socket_proxy(socket_proxy);
    let artifact = ArtifactRecord::new(
        "sha256:sweep",
        ArtifactKind::RootfsBundle,
        ArtifactSource::ExternalRegistry {
            image: "local/rootfs:sweep".to_string(),
        },
    )
    .expect("artifact");
    let bundle_dir = artifact_dir.path().join("sha256-sweep");
    let rootfs = bundle_dir.join("rootfs");
    write_busybox_rootfs(&rootfs);
    std::fs::write(
        bundle_dir.join("process.json"),
        serde_json::to_vec(&LinuxRuntimeProcessSpec {
            argv: vec!["/bin/sleep".to_string(), "300".to_string()],
            env: Vec::new(),
            workdir: "/".to_string(),
        })
        .expect("manifest json"),
    )
    .expect("manifest");

    let service_id = uuid::Uuid::now_v7();
    let deployment_id = uuid::Uuid::now_v7();
    let status = runtime
        .start(RuntimeStartRequest {
            service_name: "sweep-service".to_string(),
            service_id,
            deployment_id,
            artifact,
            internal_port: 3000,
            socket_path: runtime_dir.path().join("sweep-service/current.sock"),
            cpu_millis: 100,
            memory_bytes: 67108864,
            env: Vec::new(),
            pids_max: None,
            memory_swap_max: None,
            io_weight: None,
            replica_index: 0,
        })
        .await
        .expect("runtime start");
    wait_for_path(&status.socket_path);

    let replica_dir = runtime_dir
        .path()
        .join(service_id.to_string())
        .join(deployment_id.to_string())
        .join("0");
    assert!(replica_dir.exists(), "replica dir should exist after start");
    assert!(
        status.cgroup_path.exists(),
        "cgroup should exist after start"
    );

    // Fresh runtime over the same dirs = empty in-memory tracking, like a restart.
    let fresh =
        LinuxRuntime::new_with_paths(runtime_dir.path(), artifact_dir.path(), cgroup_root.path());
    let swept = fresh.sweep_orphans().await.expect("sweep orphans");

    assert!(
        swept >= 1,
        "expected at least one orphan swept, got {swept}"
    );
    assert!(
        !replica_dir.exists(),
        "sweep should remove the leftover replica dir {}",
        replica_dir.display()
    );
    assert!(
        !status.cgroup_path.exists(),
        "sweep should remove the leftover cgroup {}",
        status.cgroup_path.display()
    );
}

#[tokio::test]
#[ignore = "requires root, cgroup v2, Linux namespace permissions, and DENIA_PRIVILEGED_BUSYBOX_STATIC"]
async fn console_exec_reads_service_environment() {
    assert_eq!(
        std::env::var("DENIA_RUN_PRIVILEGED_TESTS").as_deref(),
        Ok("1")
    );
    assert!(
        std::fs::read_to_string("/proc/self/status")
            .expect("status")
            .lines()
            .any(|line| line == "Uid:\t0\t0\t0\t0"),
        "privileged runtime tests must run as root"
    );
    assert!(
        static_busybox().exists(),
        "static busybox must exist through DENIA_PRIVILEGED_BUSYBOX_STATIC or /usr/lib/nix/busybox"
    );
    let runtime_dir = tempfile::tempdir().expect("runtime dir");
    let artifact_dir = tempfile::tempdir().expect("artifact dir");
    let helper_dir = tempfile::tempdir().expect("helper dir");
    let cgroup_root = CgroupTestRoot::new();
    let socket_proxy = socket_proxy_helper(helper_dir.path());
    let runtime =
        LinuxRuntime::new_with_paths(runtime_dir.path(), artifact_dir.path(), cgroup_root.path())
            .with_socket_proxy(socket_proxy);
    let artifact = ArtifactRecord::new(
        "sha256:console",
        ArtifactKind::RootfsBundle,
        ArtifactSource::ExternalRegistry {
            image: "local/rootfs:console".to_string(),
        },
    )
    .expect("artifact");
    let bundle_dir = artifact_dir.path().join("sha256-console");
    let rootfs = bundle_dir.join("rootfs");
    write_busybox_rootfs(&rootfs);
    std::fs::write(
        bundle_dir.join("process.json"),
        serde_json::to_vec(&LinuxRuntimeProcessSpec {
            argv: vec!["/bin/sleep".to_string(), "300".to_string()],
            env: vec![("DENIA_CONSOLE_TEST".to_string(), "inside".to_string())],
            workdir: "/".to_string(),
        })
        .expect("manifest json"),
    )
    .expect("manifest");

    let service_id = uuid::Uuid::now_v7();
    let deployment_id = uuid::Uuid::now_v7();
    let status = runtime
        .start(RuntimeStartRequest {
            service_name: "console-service".to_string(),
            service_id,
            deployment_id,
            artifact,
            internal_port: 3000,
            socket_path: runtime_dir.path().join("console-service/current.sock"),
            cpu_millis: 100,
            memory_bytes: 67108864,
            env: Vec::new(),
            pids_max: None,
            memory_swap_max: None,
            io_weight: None,
            replica_index: 0,
        })
        .await
        .expect("runtime start");
    wait_for_path(&status.socket_path);

    let mut session = runtime
        .open_console(RuntimeConsoleRequest {
            session_id: uuid::Uuid::now_v7(),
            service_id,
            service_name: "console-service".to_string(),
            deployment_id,
            replica_index: 0,
            cols: 120,
            rows: 32,
        })
        .await
        .expect("open console");
    tokio::io::AsyncWriteExt::write_all(
        &mut session.pty,
        b"echo env=$DENIA_CONSOLE_TEST; \
          echo self_pid_ns=$(readlink /proc/self/ns/pid); \
          echo init_pid_ns=$(readlink /proc/1/ns/pid); \
          echo uid=$(id -u); \
          touch /tmp/denia-console-write-test && echo write=ok; \
          exit\n",
    )
    .await
    .expect("write console command");
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        read_pty_output(session.pty.as_mut()),
    )
    .await
    .expect("console output timeout");
    let output = String::from_utf8_lossy(&output);
    assert!(
        output.contains("env=inside"),
        "console output should contain service env, got {output:?}"
    );
    let self_pid_ns = output
        .lines()
        .find_map(|line| line.strip_prefix("self_pid_ns="))
        .expect("console output should include self pid namespace");
    let init_pid_ns = output
        .lines()
        .find_map(|line| line.strip_prefix("init_pid_ns="))
        .expect("console output should include init pid namespace");
    assert_eq!(
        self_pid_ns, init_pid_ns,
        "console shell must be born in the replica PID namespace, got {output:?}"
    );
    assert!(
        output.contains("uid=0"),
        "console shell should run as mapped root, got {output:?}"
    );
    assert!(
        output.contains("write=ok"),
        "console shell should be able to write as mapped root, got {output:?}"
    );
    runtime
        .stop(&RuntimeInstanceId {
            service_id,
            service_name: "console-service".to_string(),
            replica_index: 0,
        })
        .await
        .expect("stop service");
}

#[test]
#[ignore = "requires root, cgroup v2, Linux namespace permissions, and DENIA_PRIVILEGED_BUSYBOX_STATIC"]
fn overlay_replica_launches_with_readonly_bind_mount() {
    assert_eq!(
        std::env::var("DENIA_RUN_PRIVILEGED_TESTS").as_deref(),
        Ok("1")
    );
    assert!(
        std::fs::read_to_string("/proc/self/status")
            .expect("status")
            .lines()
            .any(|line| line == "Uid:\t0\t0\t0\t0"),
        "privileged runtime tests must run as root"
    );
    assert!(
        static_busybox().exists(),
        "static busybox must exist through DENIA_PRIVILEGED_BUSYBOX_STATIC or /usr/lib/nix/busybox"
    );

    let work_root = tempfile::tempdir().expect("overlay work root");
    let cgroup_root = CgroupTestRoot::new();
    let test_userns_base = 100000u32;

    // lower = shared read-only artifact rootfs; upper/work/merged are per-replica.
    let lower = work_root.path().join("lower");
    write_busybox_rootfs(&lower);
    let upper = work_root.path().join("upper");
    let work = work_root.path().join("work");
    let merged = work_root.path().join("merged");
    for dir in [&upper, &work, &merged] {
        fs::create_dir_all(dir).expect("overlay layer dir");
    }

    // Host file bound read-only into the guest.
    let bound_src = work_root.path().join("bound-secret");
    fs::write(&bound_src, "denia-bound-payload\n").expect("write bound source");

    let stdout_file = work_root.path().join("replica.out");
    let stderr_file = work_root.path().join("replica.err");

    // The guest reads the bound file (proving visibility) and attempts to write
    // to it (which must fail because the bind is read-only). Output records the
    // observed content and the write outcome.
    let script = "cat /.denia/bound; \
         if echo mutate > /.denia/bound 2>/dev/null; then echo WRITE_OK; else echo WRITE_DENIED; fi; \
         if head -c 1 /dev/urandom >/dev/null 2>&1 && echo discard > /dev/null; then echo DEV_OK; else echo DEV_FAIL; fi";

    let cgroup_path = cgroup_root.create_leaf("overlay-replica");
    let namespace = NamespaceConfig::new(
        lower.clone(),
        vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
    )
    .with_uid_map(test_userns_base, 65536)
    .with_cgroup_path(cgroup_path)
    .with_stdio_paths(&stdout_file, &stderr_file)
    .with_overlay(OverlaySpec {
        lower: lower.clone(),
        upper,
        work,
        merged,
    })
    .with_ro_bind(RoBind {
        src: bound_src,
        dest: PathBuf::from("/.denia/bound"),
    });

    let pid = spawn_namespaced_process(&namespace).expect("spawn overlay replica");
    let status = syscall::signal::wait(pid).expect("wait overlay replica");
    assert_eq!(
        status,
        syscall::signal::ProcessStatus::Exited(0),
        "expected overlay replica to exit successfully; stderr: {:?}",
        fs::read_to_string(&stderr_file).ok()
    );

    let output = fs::read_to_string(&stdout_file).expect("replica stdout");
    assert!(
        output.contains("denia-bound-payload"),
        "expected bound file content to be visible inside the guest, got: {output:?}"
    );
    assert!(
        output.contains("WRITE_DENIED"),
        "expected read-only bind to deny writes, got: {output:?}"
    );
    assert!(
        !output.contains("WRITE_OK"),
        "read-only bind mount must not be writable, got: {output:?}"
    );
    assert!(
        output.contains("DEV_OK"),
        "expected real /dev nodes (urandom readable, null writable), got: {output:?}"
    );
}

#[test]
#[ignore = "requires root, cgroup v2, Linux namespace permissions, and DENIA_PRIVILEGED_BUSYBOX_STATIC"]
fn overlay_replica_binds_source_under_unsearchable_dir() {
    // Regression for the "read-only bind mount errno=13 (EACCES)" failure: the
    // bind source lives under a 0700 directory the workload's mapped uid cannot
    // traverse. Applied inside the unprivileged userns the first MS_BIND fails
    // EACCES on source-path resolution; applied pre-userns (privileged, ADR-026)
    // it succeeds. Mirrors production/dev where the daemon binary used as the
    // socket-proxy source can sit under a 0700 home (e.g. a `cargo run`).
    assert_eq!(
        std::env::var("DENIA_RUN_PRIVILEGED_TESTS").as_deref(),
        Ok("1")
    );
    assert!(
        std::fs::read_to_string("/proc/self/status")
            .expect("status")
            .lines()
            .any(|line| line == "Uid:\t0\t0\t0\t0"),
        "privileged runtime tests must run as root"
    );
    assert!(
        static_busybox().exists(),
        "static busybox must exist through DENIA_PRIVILEGED_BUSYBOX_STATIC or /usr/lib/nix/busybox"
    );

    let work_root = tempfile::tempdir().expect("overlay work root");
    let cgroup_root = CgroupTestRoot::new();
    let test_userns_base = 100000u32;

    let lower = work_root.path().join("lower");
    write_busybox_rootfs(&lower);
    let upper = work_root.path().join("upper");
    let work = work_root.path().join("work");
    let merged = work_root.path().join("merged");
    for dir in [&upper, &work, &merged] {
        fs::create_dir_all(dir).expect("overlay layer dir");
    }

    // Bind source under a 0700 directory: the workload's mapped uid maps to an
    // owner unmapped in the userns, so it falls back to "other" bits (no access)
    // and source traversal is denied unless the bind runs pre-userns as root.
    let restricted = work_root.path().join("restricted");
    fs::create_dir(&restricted).expect("restricted dir");
    let bound_src = restricted.join("bound-secret");
    fs::write(&bound_src, "denia-bound-payload\n").expect("write bound source");
    fs::set_permissions(&restricted, fs::Permissions::from_mode(0o700))
        .expect("restrict bind source dir");

    let stdout_file = work_root.path().join("replica.out");
    let stderr_file = work_root.path().join("replica.err");

    let script = "cat /.denia/bound; \
         if echo mutate > /.denia/bound 2>/dev/null; then echo WRITE_OK; else echo WRITE_DENIED; fi";

    let cgroup_path = cgroup_root.create_leaf("overlay-restricted-bind");
    let namespace = NamespaceConfig::new(
        lower.clone(),
        vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
    )
    .with_uid_map(test_userns_base, 65536)
    .with_cgroup_path(cgroup_path)
    .with_stdio_paths(&stdout_file, &stderr_file)
    .with_overlay(OverlaySpec {
        lower: lower.clone(),
        upper,
        work,
        merged,
    })
    .with_ro_bind(RoBind {
        src: bound_src,
        dest: PathBuf::from("/.denia/bound"),
    });

    let pid = spawn_namespaced_process(&namespace).expect("spawn overlay replica");
    let status = syscall::signal::wait(pid).expect("wait overlay replica");
    assert_eq!(
        status,
        syscall::signal::ProcessStatus::Exited(0),
        "expected replica with bind source under 0700 dir to exit successfully; stderr: {:?}",
        fs::read_to_string(&stderr_file).ok()
    );

    let output = fs::read_to_string(&stdout_file).expect("replica stdout");
    assert!(
        output.contains("denia-bound-payload"),
        "expected bound file content to be visible inside the guest, got: {output:?}"
    );
    assert!(
        output.contains("WRITE_DENIED") && !output.contains("WRITE_OK"),
        "read-only bind mount must not be writable, got: {output:?}"
    );
}

#[test]
#[ignore = "requires root, cgroup v2, Linux namespace permissions, and DENIA_PRIVILEGED_BUSYBOX_STATIC"]
fn hardened_workload_has_no_new_privs_and_cleared_cap_bnd() {
    assert_eq!(
        std::env::var("DENIA_RUN_PRIVILEGED_TESTS").as_deref(),
        Ok("1")
    );
    assert!(
        std::fs::read_to_string("/proc/self/status")
            .expect("status")
            .lines()
            .any(|line| line == "Uid:\t0\t0\t0\t0"),
        "privileged runtime tests must run as root"
    );
    assert!(
        static_busybox().exists(),
        "static busybox must exist through DENIA_PRIVILEGED_BUSYBOX_STATIC or /usr/lib/nix/busybox"
    );

    let artifact_dir = tempfile::tempdir().expect("artifact dir");
    let cgroup_root = CgroupTestRoot::new();

    let test_userns_base = 100000u32;
    let bundle_dir = artifact_dir.path().join("sha256-hardened");
    let rootfs = bundle_dir.join("rootfs");
    write_busybox_rootfs(&rootfs);
    let status_file = artifact_dir.path().join("self-status");
    let stderr_file = artifact_dir.path().join("self-status.err");

    let cgroup_path = cgroup_root.create_leaf("hardened-svc");
    let namespace = NamespaceConfig::new(
        rootfs.clone(),
        vec!["/bin/cat".to_string(), "/proc/self/status".to_string()],
    )
    .with_uid_map(test_userns_base, 65536)
    .with_cgroup_path(cgroup_path)
    .with_stdio_paths(&status_file, &stderr_file);
    let pid = spawn_namespaced_process(&namespace).expect("spawn namespaced process");
    let status = syscall::signal::wait(pid).expect("wait namespaced process");
    assert_eq!(
        status,
        syscall::signal::ProcessStatus::Exited(0),
        "expected hardened workload to exit successfully"
    );

    let proc_status =
        std::fs::read_to_string(&status_file).expect("workload /proc/self/status output");

    let nspid = proc_status
        .lines()
        .find(|line| line.starts_with("NSpid:"))
        .expect("NSpid field");
    let namespace_pid = nspid
        .split_whitespace()
        .last()
        .expect("NSpid namespace pid");
    assert_eq!(
        namespace_pid, "1",
        "expected direct workload process to run as pid 1 in the new PID namespace, got: {nspid}"
    );

    let no_new_privs = proc_status
        .lines()
        .find(|line| line.starts_with("NoNewPrivs:"))
        .expect("NoNewPrivs field");
    let (_, no_new_privs_value) = no_new_privs
        .split_once('\t')
        .expect("NoNewPrivs tab-separated");
    assert_eq!(
        no_new_privs_value.trim(),
        "1",
        "expected NoNewPrivs: 1, got: {no_new_privs}"
    );

    let cap_bnd = proc_status
        .lines()
        .find(|line| line.starts_with("CapBnd:"))
        .expect("CapBnd field");
    let (_, cap_bnd_value) = cap_bnd.split_once('\t').expect("CapBnd tab-separated");
    assert_eq!(
        cap_bnd_value.trim(),
        "0000000000000000",
        "expected CapBnd cleared (all zeros), got: {cap_bnd_value}"
    );
}

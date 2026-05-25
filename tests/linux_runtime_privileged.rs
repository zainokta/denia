use denia::{
    artifacts::{ArtifactKind, ArtifactRecord, ArtifactSource},
    domain::RuntimeStartRequest,
    runtime::{LinuxRuntime, LinuxRuntimeProcessSpec, Runtime},
};
use std::{
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};

fn static_busybox() -> PathBuf {
    std::env::var_os("DENIA_PRIVILEGED_BUSYBOX_STATIC")
        .map(PathBuf::from)
        .expect("DENIA_PRIVILEGED_BUSYBOX_STATIC must point to a static busybox binary")
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

#[test]
#[ignore = "requires root, cgroup v2, and Linux namespace permissions"]
fn privileged_runtime_tests_are_explicitly_gated() {
    assert_eq!(
        std::env::var("DENIA_RUN_PRIVILEGED_TESTS").as_deref(),
        Ok("1")
    );
}

#[tokio::test]
#[ignore = "requires root, cgroup v2, Linux namespace permissions, setpriv, and DENIA_PRIVILEGED_BUSYBOX_STATIC"]
async fn linux_runtime_start_uses_unshare_and_cgroup_gate() {
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
        "DENIA_PRIVILEGED_BUSYBOX_STATIC must exist"
    );

    let runtime_dir = tempfile::tempdir().expect("runtime dir");
    let artifact_dir = tempfile::tempdir().expect("artifact dir");
    let cgroup_root = tempfile::tempdir().expect("cgroup dir");
    let runtime =
        LinuxRuntime::new_with_paths(runtime_dir.path(), artifact_dir.path(), cgroup_root.path());
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
        })
        .await
        .expect("runtime start");

    assert_eq!(status.state, "running");
    assert!(status.pid.is_some());
    runtime
        .stop("true-service")
        .await
        .expect("stop runtime process");
    assert_eq!(
        std::fs::read_to_string(
            cgroup_root
                .path()
                .join("true-service")
                .join(deployment_id.to_string())
                .join("cpu.max")
        )
        .expect("cpu.max"),
        "10000 100000\n"
    );
}

#[tokio::test]
#[ignore = "requires root, cgroup v2, Linux namespace permissions, setpriv, and DENIA_PRIVILEGED_BUSYBOX_STATIC"]
async fn hardened_workload_has_no_new_privs_and_cleared_cap_bnd() {
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
        std::process::Command::new("which")
            .arg("setpriv")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false),
        "setpriv must be available on PATH for this test"
    );
    assert!(
        static_busybox().exists(),
        "DENIA_PRIVILEGED_BUSYBOX_STATIC must exist"
    );

    let runtime_dir = tempfile::tempdir().expect("runtime dir");
    let artifact_dir = tempfile::tempdir().expect("artifact dir");
    let cgroup_root = tempfile::tempdir().expect("cgroup dir");

    let test_userns_base = 100000u32;
    let runtime =
        LinuxRuntime::new_with_paths(runtime_dir.path(), artifact_dir.path(), cgroup_root.path())
            .with_userns(test_userns_base, 65536)
            .with_setpriv("setpriv");

    let artifact = ArtifactRecord::new(
        "sha256:hardened",
        ArtifactKind::RootfsBundle,
        ArtifactSource::ExternalRegistry {
            image: "local/rootfs:hardened".to_string(),
        },
    )
    .expect("artifact");
    let bundle_dir = artifact_dir.path().join("sha256-hardened");
    let rootfs = bundle_dir.join("rootfs");
    write_busybox_rootfs(&rootfs);
    let output_dir = rootfs.join("denia-output");
    std::fs::create_dir_all(&output_dir).expect("output dir");
    std::process::Command::new("chown")
        .args([
            format!("{test_userns_base}:{test_userns_base}"),
            output_dir.to_string_lossy().into_owned(),
        ])
        .status()
        .expect("chown output dir");
    let status_file = output_dir.join("self-status");
    std::fs::write(
        bundle_dir.join("process.json"),
        serde_json::to_vec(&LinuxRuntimeProcessSpec {
            argv: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "cat /proc/self/status > /denia-output/self-status".to_string(),
            ],
            env: Vec::new(),
            workdir: "/".to_string(),
        })
        .expect("manifest json"),
    )
    .expect("manifest");

    let deployment_id = uuid::Uuid::now_v7();
    let status = runtime
        .start(RuntimeStartRequest {
            service_name: "hardened-svc".to_string(),
            service_id: uuid::Uuid::now_v7(),
            deployment_id,
            artifact,
            internal_port: 3001,
            socket_path: runtime_dir.path().join("hardened-svc/current.sock"),
            cpu_millis: 100,
            memory_bytes: 67108864,
        })
        .await
        .expect("runtime start");

    assert_eq!(status.state, "running");
    assert!(status.pid.is_some());

    std::thread::sleep(std::time::Duration::from_secs(2));
    runtime
        .stop("hardened-svc")
        .await
        .expect("stop hardened runtime process");

    let proc_status =
        std::fs::read_to_string(&status_file).expect("workload /proc/self/status output");

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

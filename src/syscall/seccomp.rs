use crate::syscall::SyscallError;

const AUDIT_ARCH_X86_64: u32 = 0xC000003E;
#[allow(dead_code)]
const AUDIT_ARCH_AARCH64: u32 = 0xC00000B7;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct SockFilter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

#[repr(C)]
struct SockFprog {
    len: u16,
    filter: *const SockFilter,
}

fn stmt(code: u16, k: u32) -> SockFilter {
    SockFilter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

fn jump(code: u16, k: u32, jt: u8, jf: u8) -> SockFilter {
    SockFilter { code, jt, jf, k }
}

fn deny_errno(errno: u16) -> u32 {
    0x0005_0000 | (errno as u32)
}

// `seccomp(2)` operation + flag constants (libc does not expose these on all
// targets). `SECCOMP_FILTER_FLAG_TSYNC` synchronizes the installed filter onto
// every thread of the calling process, not just the caller — required so the
// filter covers the socket-proxy's other Tokio worker threads (M1), and a no-op
// (but harmless) for the single-threaded workload/job child.
const SECCOMP_SET_MODE_FILTER: libc::c_uint = 1;
const SECCOMP_FILTER_FLAG_TSYNC: libc::c_ulong = 1;

pub fn install_filter() -> Result<(), SyscallError> {
    let syscalls = denylist();
    let program = build_program(&syscalls);
    let prog = SockFprog {
        len: program.len() as u16,
        filter: program.as_ptr(),
    };
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) < 0 {
            return Err(SyscallError::Seccomp(format!(
                "PR_SET_NO_NEW_PRIVS: {}",
                std::io::Error::last_os_error()
            )));
        }
        // Prefer the `seccomp(2)` syscall with TSYNC so the filter applies to ALL
        // threads of the process. `prctl(PR_SET_SECCOMP)` cannot take flags and
        // would filter only the calling thread, leaving the proxy's other worker
        // threads unfiltered. On a kernel without `seccomp(2)` (< 3.17) or where
        // TSYNC fails (a thread already has an incompatible filter — it does not
        // here, this is the first install), fall back to per-thread `prctl`.
        let rc = libc::syscall(
            libc::SYS_seccomp,
            SECCOMP_SET_MODE_FILTER,
            SECCOMP_FILTER_FLAG_TSYNC,
            &prog as *const SockFprog,
        );
        if rc == 0 {
            return Ok(());
        }
        let seccomp_errno = std::io::Error::last_os_error();
        let recoverable = matches!(
            seccomp_errno.raw_os_error(),
            Some(libc::ENOSYS) | Some(libc::EINVAL)
        );
        if !recoverable {
            return Err(SyscallError::Seccomp(format!(
                "seccomp(SET_MODE_FILTER, TSYNC): {seccomp_errno}"
            )));
        }
        if libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            &prog as *const SockFprog,
        ) < 0
        {
            return Err(SyscallError::Seccomp(format!(
                "PR_SET_SECCOMP: {}",
                std::io::Error::last_os_error()
            )));
        }
    }
    Ok(())
}

fn build_program(syscalls: &[u32]) -> Vec<SockFilter> {
    let count = syscalls.len();
    let mut p = Vec::with_capacity(5 + count);
    #[cfg(target_arch = "x86_64")]
    let arch = AUDIT_ARCH_X86_64;
    #[cfg(target_arch = "aarch64")]
    let arch = AUDIT_ARCH_AARCH64;
    p.push(stmt((libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16, 4));
    p.push(jump(
        (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
        arch,
        0,
        count as u8 + 2,
    ));
    p.push(stmt((libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16, 0));
    for (i, &nr) in syscalls.iter().enumerate() {
        let remaining = (count - i - 1) as u8;
        p.push(jump(
            (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            nr,
            remaining + 1,
            0,
        ));
    }
    p.push(stmt(
        (libc::BPF_RET | libc::BPF_K) as u16,
        libc::SECCOMP_RET_ALLOW,
    ));
    p.push(stmt(
        (libc::BPF_RET | libc::BPF_K) as u16,
        deny_errno(libc::EPERM as u16),
    ));
    p
}

fn denylist() -> Vec<u32> {
    // Use libc::SYS_* constants rather than hand-written numbers: the constants
    // are correct for each target architecture, so the filter can never silently
    // block (or miss) the wrong syscall due to a transposed number.
    let mut s: Vec<libc::c_long> = Vec::new();
    #[cfg(target_arch = "x86_64")]
    {
        s.extend_from_slice(&[
            libc::SYS_ptrace,
            libc::SYS_sethostname, // redundant with UTS ns, defense-in-depth
            libc::SYS_setsid,
            libc::SYS_mount,
            libc::SYS_umount2,
            libc::SYS_init_module,
            libc::SYS_delete_module,
            libc::SYS_kexec_load,
            libc::SYS_kexec_file_load,
            libc::SYS_mknod,
            libc::SYS_mknodat,
            libc::SYS_uselib,
            libc::SYS_swapon,
            libc::SYS_swapoff,
            libc::SYS_nfsservctl,
            libc::SYS_putpmsg, // unused, attack surface reduction
            libc::SYS_kcmp,
            libc::SYS_finit_module,
            libc::SYS_bpf,
            libc::SYS_open_tree,
            libc::SYS_move_mount,
            libc::SYS_fsopen,
            libc::SYS_fsconfig,
            libc::SYS_fsmount,
            libc::SYS_memfd_create,
        ]);
    }
    #[cfg(target_arch = "aarch64")]
    {
        s.extend_from_slice(&[
            libc::SYS_ptrace,
            libc::SYS_sethostname,
            libc::SYS_setsid,
            libc::SYS_mount,
            libc::SYS_umount2,
            libc::SYS_init_module,
            libc::SYS_delete_module,
            libc::SYS_kexec_load,
            libc::SYS_kexec_file_load,
            libc::SYS_mknodat,
            libc::SYS_swapon,
            libc::SYS_swapoff,
            libc::SYS_kcmp,
            libc::SYS_finit_module,
            libc::SYS_bpf,
            libc::SYS_open_tree,
            libc::SYS_move_mount,
            libc::SYS_fsopen,
            libc::SYS_fsconfig,
            libc::SYS_fsmount,
            libc::SYS_memfd_create,
        ]);
    }
    // Escape-relevant syscalls common to both arches (M2). Not an exhaustive
    // default-deny allowlist — that remains deferred per ADR-005 — but these
    // close the highest-value gaps the review flagged. `no_new_privs` + the empty
    // bounding set already blunt most, this adds an explicit EPERM:
    //   keyctl/add_key/request_key  kernel keyring manipulation
    //   unshare/setns               new-namespace creation / joining by the workload
    //   userfaultfd                 page-fault handling primitive used in kernel exploits
    //   perf_event_open             broad kernel attack surface
    //   io_uring_setup              large async-syscall attack surface, rarely needed
    //   process_vm_readv/writev     cross-process memory access
    //   quotactl, acct              privileged fs/accounting controls
    //   seccomp                     block re-filtering (bpf is already denied)
    // `clone`/`clone3` are intentionally NOT denied: glibc fork() may route
    // through clone3, so blocking it would break the workload's own process
    // spawning; namespace creation via clone flags is instead curtailed by the
    // empty bounding set + no_new_privs and the unshare/setns denials.
    s.extend_from_slice(&[
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_unshare,
        libc::SYS_setns,
        libc::SYS_userfaultfd,
        libc::SYS_perf_event_open,
        libc::SYS_io_uring_setup,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_quotactl,
        libc::SYS_acct,
        libc::SYS_seccomp,
    ]);
    s.into_iter().map(|nr| nr as u32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_structure_has_expected_layout() {
        let syscalls = denylist();
        let program = build_program(&syscalls);
        assert_eq!(program.len(), 5 + syscalls.len());
        assert_eq!(
            program[0].code,
            (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16
        );
        assert_eq!(program[0].k, 4);
        let last = program.len() - 1;
        assert_eq!(program[last].k, deny_errno(libc::EPERM as u16));
        assert_eq!(program[last - 1].k, libc::SECCOMP_RET_ALLOW);
    }

    #[test]
    fn denylist_is_non_empty_on_supported_arch() {
        let syscalls = denylist();
        assert!(!syscalls.is_empty());
    }

    #[test]
    fn denylist_blocks_mount_family_with_correct_numbers() {
        let syscalls = denylist();
        for nr in [
            libc::SYS_mount,
            libc::SYS_umount2,
            libc::SYS_ptrace,
            libc::SYS_bpf,
            libc::SYS_init_module,
        ] {
            assert!(
                syscalls.contains(&(nr as u32)),
                "denylist missing syscall {nr}"
            );
        }
    }

    #[test]
    fn denylist_blocks_escape_relevant_syscalls() {
        let syscalls = denylist();
        for nr in [
            libc::SYS_keyctl,
            libc::SYS_add_key,
            libc::SYS_request_key,
            libc::SYS_unshare,
            libc::SYS_setns,
            libc::SYS_userfaultfd,
            libc::SYS_perf_event_open,
            libc::SYS_io_uring_setup,
            libc::SYS_process_vm_readv,
            libc::SYS_process_vm_writev,
            libc::SYS_quotactl,
            libc::SYS_acct,
            libc::SYS_seccomp,
        ] {
            assert!(
                syscalls.contains(&(nr as u32)),
                "denylist missing escape-relevant syscall {nr}"
            );
        }
    }

    #[test]
    fn deny_errno_encodes_action_and_value() {
        let v = deny_errno(libc::EPERM as u16);
        assert_eq!(v & 0xFFFF_0000, 0x0005_0000);
        assert_eq!(v & 0xFFFF, libc::EPERM as u32);
    }
}

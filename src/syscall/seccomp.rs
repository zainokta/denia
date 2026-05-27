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
    fn deny_errno_encodes_action_and_value() {
        let v = deny_errno(libc::EPERM as u16);
        assert_eq!(v & 0xFFFF_0000, 0x0005_0000);
        assert_eq!(v & 0xFFFF, libc::EPERM as u32);
    }
}

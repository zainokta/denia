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
    let mut s = Vec::new();
    #[cfg(target_arch = "x86_64")]
    {
        s.extend_from_slice(&[
            101, // ptrace
            139, // sethostname (redundant with UTS ns, defense-in-depth)
            156, // setsid
            177, // mount
            178, // umount2
            175, // init_module
            176, // delete_module
            246, // kexec_load
            298, // kexec_file_load
            133, // mknod
            259, // mknodat
            154, // uselib
            180, // swapon
            167, // swapoff
            169, // nfsservctl
            206, // putpmsg (unused, attack surface reduction)
            304, // kcmp
            313, // finit_module
            321, // bpf
            401, // open_tree
            402, // move_mount
            403, // fsopen
            404, // fsconfig
            405, // fsmount
            312, // memfd_create
        ]);
    }
    #[cfg(target_arch = "aarch64")]
    {
        s.extend_from_slice(&[
            117, // ptrace
            160, // sethostname
            157, // setsid
            40,  // mount
            39,  // umount2
            105, // init_module
            106, // delete_module
            107, // kexec_load
            294, // kexec_file_load
            34,  // mknodat
            224, // uselib (not present on all aarch64 kernels, harmless if absent)
            225, // swapon
            226, // swapoff
            180, // nfsservctl
            282, // kcmp
            273, // finit_module
            280, // bpf
            43,  // open_tree
            429, // move_mount
            430, // fsopen
            431, // fsconfig
            432, // fsmount
            279, // memfd_create
        ]);
    }
    s
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
    fn deny_errno_encodes_action_and_value() {
        let v = deny_errno(libc::EPERM as u16);
        assert_eq!(v & 0xFFFF_0000, 0x0005_0000);
        assert_eq!(v & 0xFFFF, libc::EPERM as u32);
    }
}

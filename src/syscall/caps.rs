use rustix::thread;

use crate::syscall::SyscallError;

pub fn set_no_new_privs() -> Result<(), SyscallError> {
    thread::set_no_new_privs(true)
        .map_err(|e| SyscallError::Capability(format!("PR_SET_NO_NEW_PRIVS: {e}")))
}

pub fn drop_bounding_caps() -> Result<(), SyscallError> {
    for cap in ALL_CAPABILITIES {
        thread::remove_capability_from_bounding_set(cap).map_err(|e| {
            SyscallError::Capability(format!("drop bounding capability {cap:?}: {e}"))
        })?;
    }
    Ok(())
}

const ALL_CAPABILITIES: [thread::CapabilitySet; 41] = [
    thread::CapabilitySet::CHOWN,
    thread::CapabilitySet::DAC_OVERRIDE,
    thread::CapabilitySet::DAC_READ_SEARCH,
    thread::CapabilitySet::FOWNER,
    thread::CapabilitySet::FSETID,
    thread::CapabilitySet::KILL,
    thread::CapabilitySet::SETGID,
    thread::CapabilitySet::SETUID,
    thread::CapabilitySet::SETPCAP,
    thread::CapabilitySet::LINUX_IMMUTABLE,
    thread::CapabilitySet::NET_BIND_SERVICE,
    thread::CapabilitySet::NET_BROADCAST,
    thread::CapabilitySet::NET_ADMIN,
    thread::CapabilitySet::NET_RAW,
    thread::CapabilitySet::IPC_LOCK,
    thread::CapabilitySet::IPC_OWNER,
    thread::CapabilitySet::SYS_MODULE,
    thread::CapabilitySet::SYS_RAWIO,
    thread::CapabilitySet::SYS_CHROOT,
    thread::CapabilitySet::SYS_PTRACE,
    thread::CapabilitySet::SYS_PACCT,
    thread::CapabilitySet::SYS_ADMIN,
    thread::CapabilitySet::SYS_BOOT,
    thread::CapabilitySet::SYS_NICE,
    thread::CapabilitySet::SYS_RESOURCE,
    thread::CapabilitySet::SYS_TIME,
    thread::CapabilitySet::SYS_TTY_CONFIG,
    thread::CapabilitySet::MKNOD,
    thread::CapabilitySet::LEASE,
    thread::CapabilitySet::AUDIT_WRITE,
    thread::CapabilitySet::AUDIT_CONTROL,
    thread::CapabilitySet::SETFCAP,
    thread::CapabilitySet::MAC_OVERRIDE,
    thread::CapabilitySet::MAC_ADMIN,
    thread::CapabilitySet::SYSLOG,
    thread::CapabilitySet::WAKE_ALARM,
    thread::CapabilitySet::BLOCK_SUSPEND,
    thread::CapabilitySet::AUDIT_READ,
    thread::CapabilitySet::PERFMON,
    thread::CapabilitySet::BPF,
    thread::CapabilitySet::CHECKPOINT_RESTORE,
];

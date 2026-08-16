//! seccomp-bpf policy for the guest agent.
//!
//! A seccomp filter installed by the agent is inherited by every process it
//! spawns, so this one filter hardens the whole guest: nothing under the
//! agent — the workload, exec sessions, or any of their children — may create
//! a raw packet or raw IP socket. Raw sockets are the classic privilege-
//! escalation surface when images or workloads are not fully trusted.
//!
//! The always-on filter is deliberately narrow: it inspects only `socket(2)`
//! and denies raw packet/IP sockets. Strict mode adds workload-scoped denies
//! for kernel-control and namespace-management syscalls while leaving the
//! agent unrestricted.
//!
//! It is installed with `SECCOMP_MODE_FILTER`. The agent is guest PID 1 and
//! never execs a setuid binary, so it needs no `no_new_privs`; not setting it
//! leaves the workload free to keep setuid bits in its rootfs. An unexpected
//! arch (syscall-number mismatch would be misinterpreted otherwise) is killed
//! outright.

// --- classic BPF opcodes (linux/bpf_common.h) ---
const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JMP_JEQ_K: u16 = 0x15;
const BPF_ALU_AND_K: u16 = 0x54;
const BPF_RET_K: u16 = 0x06;

// --- seccomp (linux/seccomp.h, linux/audit.h) ---
const PR_SET_SECCOMP: i32 = 22;
const SECCOMP_MODE_FILTER: i32 = 2;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const EPERM: u32 = 1;

// The arch a seccomp_data.arch must carry. Guests are same-arch as the host,
// so the agent never runs on more than one.
#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;

// socket(2) syscall number (arch-specific).
#[cfg(target_arch = "x86_64")]
const SYS_SOCKET: u32 = 41;
#[cfg(target_arch = "aarch64")]
const SYS_SOCKET: u32 = 198;

// High-risk syscalls denied in strict mode (arch-specific numbers).
#[cfg(target_arch = "x86_64")]
const SYS_BPF: u32 = 321;
#[cfg(target_arch = "aarch64")]
const SYS_BPF: u32 = 280;
#[cfg(target_arch = "x86_64")]
const SYS_KEYCTL: u32 = 250;
#[cfg(target_arch = "aarch64")]
const SYS_KEYCTL: u32 = 219;
#[cfg(target_arch = "x86_64")]
const SYS_PERF_EVENT_OPEN: u32 = 298;
#[cfg(target_arch = "aarch64")]
const SYS_PERF_EVENT_OPEN: u32 = 241;
#[cfg(target_arch = "x86_64")]
const SYS_USERFAULTFD: u32 = 323;
#[cfg(target_arch = "aarch64")]
const SYS_USERFAULTFD: u32 = 282;
// io_uring numbers are the same on both arches.
const SYS_IO_URING_SETUP: u32 = 425;
const SYS_IO_URING_ENTER: u32 = 426;
const SYS_IO_URING_REGISTER: u32 = 427;

// Namespace, filesystem, module, and kernel-loading syscalls denied in
// strict mode. Values are from the supported Linux architectures' syscall
// tables; these are syscall ABI numbers, not host libc definitions.
#[cfg(target_arch = "x86_64")]
const SYS_PTRACE: u32 = 101;
#[cfg(target_arch = "aarch64")]
const SYS_PTRACE: u32 = 117;
#[cfg(target_arch = "x86_64")]
const SYS_MOUNT: u32 = 165;
#[cfg(target_arch = "aarch64")]
const SYS_MOUNT: u32 = 40;
#[cfg(target_arch = "x86_64")]
const SYS_UMOUNT2: u32 = 166;
#[cfg(target_arch = "aarch64")]
const SYS_UMOUNT2: u32 = 39;
#[cfg(target_arch = "x86_64")]
const SYS_PIVOT_ROOT: u32 = 155;
#[cfg(target_arch = "aarch64")]
const SYS_PIVOT_ROOT: u32 = 41;
#[cfg(target_arch = "x86_64")]
const SYS_UNSHARE: u32 = 272;
#[cfg(target_arch = "aarch64")]
const SYS_UNSHARE: u32 = 97;
#[cfg(target_arch = "x86_64")]
const SYS_SETNS: u32 = 308;
#[cfg(target_arch = "aarch64")]
const SYS_SETNS: u32 = 268;
#[cfg(target_arch = "x86_64")]
const SYS_INIT_MODULE: u32 = 175;
#[cfg(target_arch = "aarch64")]
const SYS_INIT_MODULE: u32 = 105;
#[cfg(target_arch = "x86_64")]
const SYS_DELETE_MODULE: u32 = 176;
#[cfg(target_arch = "aarch64")]
const SYS_DELETE_MODULE: u32 = 106;
#[cfg(target_arch = "x86_64")]
const SYS_FINIT_MODULE: u32 = 313;
#[cfg(target_arch = "aarch64")]
const SYS_FINIT_MODULE: u32 = 273;
#[cfg(target_arch = "x86_64")]
const SYS_KEXEC_LOAD: u32 = 246;
#[cfg(target_arch = "aarch64")]
const SYS_KEXEC_LOAD: u32 = 104;
#[cfg(target_arch = "x86_64")]
const SYS_KEXEC_FILE_LOAD: u32 = 320;
#[cfg(target_arch = "aarch64")]
const SYS_KEXEC_FILE_LOAD: u32 = 294;

// Offsets into struct seccomp_data.
const OFF_SYSCALL: u32 = 0;
const OFF_ARCH: u32 = 4;
const OFF_ARG0: u32 = 16; // socket(2) domain
const OFF_ARG1: u32 = 24; // socket(2) type

// Socket ABI; identical on the supported arches.
const AF_INET: u32 = 2;
const AF_INET6: u32 = 10;
const AF_PACKET: u32 = 17;
const SOCK_RAW: u32 = 3;
// AF_* fit in 32 bits; SOCK_* live in the low 4 bits of `type`, with
// NONBLOCK/CLOEXEC above them, so mask before comparing.
const SOCK_TYPE_MASK: u32 = 0xf;

/// Label for a forward jump target inside the filter program.
#[derive(Clone, Copy)]
enum Label {
    Allow,
    Deny,
    TypeCheck,
    Kill,
}

struct Filter {
    insns: Vec<libc::sock_filter>,
    /// Unresolved jumps: (instruction index, true=jt / false=jf, target).
    pending: Vec<(usize, bool, Label)>,
}

impl Filter {
    fn ld_abs(&mut self, off: u32) -> usize {
        self.push(libc::sock_filter {
            code: BPF_LD_W_ABS,
            jt: 0,
            jf: 0,
            k: off,
        })
    }

    fn and(&mut self, mask: u32) {
        self.push(libc::sock_filter {
            code: BPF_ALU_AND_K,
            jt: 0,
            jf: 0,
            k: mask,
        });
    }

    /// Jump-if-equal; `None` falls through to the next instruction.
    fn jeq(&mut self, k: u32, jt: Option<Label>, jf: Option<Label>) {
        let idx = self.push(libc::sock_filter {
            code: BPF_JMP_JEQ_K,
            jt: 0,
            jf: 0,
            k,
        });
        for (target, is_jt) in [(jt, true), (jf, false)] {
            if let Some(label) = target {
                self.pending.push((idx, is_jt, label));
            }
        }
    }

    fn ret(&mut self, k: u32) -> usize {
        self.push(libc::sock_filter {
            code: BPF_RET_K,
            jt: 0,
            jf: 0,
            k,
        })
    }

    fn push(&mut self, insn: libc::sock_filter) -> usize {
        self.insns.push(insn);
        self.insns.len() - 1
    }
}

fn build() -> Vec<libc::sock_filter> {
    let mut f = Filter {
        insns: Vec::new(),
        pending: Vec::new(),
    };

    // 0: load arch
    f.ld_abs(OFF_ARCH);
    // 1: not the expected arch → kill (a wrong arch means the syscall
    //    number below is interpreted in the wrong table).
    f.jeq(AUDIT_ARCH, None, Some(Label::Kill));
    // 2: load syscall number
    f.ld_abs(OFF_SYSCALL);
    // 3: not socket(2) → allow
    f.jeq(SYS_SOCKET, None, Some(Label::Allow));
    // 4: load domain
    f.ld_abs(OFF_ARG0);
    // 5: AF_PACKET, any type → deny
    f.jeq(AF_PACKET, Some(Label::Deny), None);
    // 6, 7: AF_INET / AF_INET6 → check the type; anything else → allow
    f.jeq(AF_INET, Some(Label::TypeCheck), None);
    f.jeq(AF_INET6, Some(Label::TypeCheck), Some(Label::Allow));
    // 8, 9, 10: type (masked to the SOCK_* bits) == SOCK_RAW → deny
    let type_check = f.ld_abs(OFF_ARG1);
    f.and(SOCK_TYPE_MASK);
    f.jeq(SOCK_RAW, Some(Label::Deny), Some(Label::Allow));
    // 11, 12, 13: terminal returns
    let allow = f.ret(SECCOMP_RET_ALLOW);
    let deny = f.ret(SECCOMP_RET_ERRNO | EPERM);
    let kill = f.ret(SECCOMP_RET_KILL_PROCESS);

    for (idx, is_jt, label) in &f.pending {
        let target = match *label {
            Label::Allow => allow,
            Label::Deny => deny,
            Label::TypeCheck => type_check,
            Label::Kill => kill,
        };
        let off = target as i64 - *idx as i64 - 1;
        assert!(
            (0..=u8::MAX as i64).contains(&off),
            "seccomp jump out of range"
        );
        let off = off as u8;
        if *is_jt {
            f.insns[*idx].jt = off;
        } else {
            f.insns[*idx].jf = off;
        }
    }

    f.insns
}

/// Install a seccomp filter on the calling process. The filter applies to
/// every process spawned from here on; it can never be removed or weakened
/// by a child.
fn install(prog: Vec<libc::sock_filter>) -> Result<(), std::io::Error> {
    let mut fprog = libc::sock_fprog {
        len: prog.len() as u16,
        filter: prog.as_ptr().cast_mut(),
    };
    let rc = unsafe {
        libc::prctl(
            PR_SET_SECCOMP,
            SECCOMP_MODE_FILTER,
            &mut fprog as *mut libc::sock_fprog,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Install the raw-socket ban. Failure is fatal: a guest that cannot be
/// sandboxed will not be trusted to run.
pub fn install_raw_socket_filter() -> Result<(), std::io::Error> {
    install(build())
}

/// Build the strict-mode filter: deny a fixed set of high-risk syscalls
/// (`bpf`, namespace/mount controls, module loading, kexec, `ptrace`,
/// `keyctl`, `perf_event_open`, `userfaultfd`, `io_uring_*`) while
/// letting everything else pass. Returns `EPERM` for the denied set (the
/// same action as the raw-socket ban: a denied call is an error, not a
/// silent pass), and kills on an unexpected arch like the raw-socket filter.
///
/// Unlike the always-on raw-socket filter, this one is *workload-scoped*:
/// the agent installs it in the workload's `pre_exec` (see
/// `apply_strict_seccomp` in linux.rs), so the agent itself keeps the full
/// syscall surface it needs for exec/pty plumbing while the workload and
/// everything it spawns lose the high-risk kernel interfaces.
fn build_strict() -> Vec<libc::sock_filter> {
    let mut f = Filter {
        insns: Vec::new(),
        pending: Vec::new(),
    };

    // 0: load arch
    f.ld_abs(OFF_ARCH);
    // 1: not the expected arch → kill
    f.jeq(AUDIT_ARCH, None, Some(Label::Kill));
    // 2: load syscall number
    f.ld_abs(OFF_SYSCALL);
    // 3..: denied syscalls → deny, else fall through to allow
    for sys in [
        SYS_BPF,
        SYS_KEYCTL,
        SYS_PERF_EVENT_OPEN,
        SYS_USERFAULTFD,
        SYS_IO_URING_SETUP,
        SYS_IO_URING_ENTER,
        SYS_IO_URING_REGISTER,
        SYS_PTRACE,
        SYS_MOUNT,
        SYS_UMOUNT2,
        SYS_PIVOT_ROOT,
        SYS_UNSHARE,
        SYS_SETNS,
        SYS_INIT_MODULE,
        SYS_DELETE_MODULE,
        SYS_FINIT_MODULE,
        SYS_KEXEC_LOAD,
        SYS_KEXEC_FILE_LOAD,
    ] {
        f.jeq(sys, Some(Label::Deny), None);
    }
    // terminals
    let allow = f.ret(SECCOMP_RET_ALLOW);
    let deny = f.ret(SECCOMP_RET_ERRNO | EPERM);
    let kill = f.ret(SECCOMP_RET_KILL_PROCESS);

    for (idx, is_jt, label) in &f.pending {
        let target = match *label {
            Label::Allow => allow,
            Label::Deny => deny,
            Label::TypeCheck => unreachable!("strict filter has no type check"),
            Label::Kill => kill,
        };
        let off = target as i64 - *idx as i64 - 1;
        assert!(
            (0..=u8::MAX as i64).contains(&off),
            "seccomp jump out of range"
        );
        let off = off as u8;
        if *is_jt {
            f.insns[*idx].jt = off;
        } else {
            f.insns[*idx].jf = off;
        }
    }

    f.insns
}

/// Install the strict-mode filter on the calling process. Used from the
/// workload's `pre_exec` hook, so it is inherited by the workload and every
/// process it spawns.
pub fn install_strict_filter() -> Result<(), std::io::Error> {
    install(build_strict())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal classic-BPF interpreter over a fake `seccomp_data`. Runs the
    /// generated program against (arch, syscall, arg0, arg1) and returns the
    /// SECCOMP_RET_* action (or the raw result for ERRNO). This exercises the
    /// exact instruction stream the kernel would execute, so the deny/allow
    /// mapping is verified without needing an actual seccomp install.
    fn run(prog: &[libc::sock_filter], arch: u32, syscall: u32, arg0: u32, arg1: u32) -> u32 {
        let mut pc = 0usize;
        let mut acc = 0u32;
        loop {
            let insn = &prog[pc];
            match insn.code {
                BPF_LD_W_ABS => {
                    acc = match insn.k {
                        OFF_ARCH => arch,
                        OFF_SYSCALL => syscall,
                        OFF_ARG0 => arg0,
                        OFF_ARG1 => arg1,
                        _ => panic!("unknown abs offset {}", insn.k),
                    };
                    pc += 1;
                }
                BPF_JMP_JEQ_K => {
                    let take = acc == insn.k;
                    let off = if take { insn.jt } else { insn.jf };
                    pc = if off > 0 { pc + 1 + off as usize } else { pc + 1 };
                }
                BPF_ALU_AND_K => {
                    acc &= insn.k;
                    pc += 1;
                }
                BPF_RET_K => return insn.k,
                other => panic!("unexpected opcode {other:#x}"),
            }
        }
    }

    fn assert_action(prog: &[libc::sock_filter], syscall: u32, want: u32) {
        let got = run(prog, AUDIT_ARCH, syscall, 0, 0);
        assert_eq!(got, want, "syscall {syscall}");
    }

    #[test]
    fn strict_denies_high_risk_syscalls_and_allows_others() {
        let prog = build_strict();
        let deny = SECCOMP_RET_ERRNO | EPERM;
        for sys in [
            SYS_BPF,
            SYS_KEYCTL,
            SYS_PERF_EVENT_OPEN,
            SYS_USERFAULTFD,
            SYS_IO_URING_SETUP,
            SYS_IO_URING_ENTER,
            SYS_IO_URING_REGISTER,
            SYS_PTRACE,
            SYS_MOUNT,
            SYS_UMOUNT2,
            SYS_PIVOT_ROOT,
            SYS_UNSHARE,
            SYS_SETNS,
            SYS_INIT_MODULE,
            SYS_DELETE_MODULE,
            SYS_FINIT_MODULE,
            SYS_KEXEC_LOAD,
            SYS_KEXEC_FILE_LOAD,
        ] {
            assert_action(&prog, sys, deny);
        }
        // Ordinary syscalls pass untouched (read/write/execve/socket...).
        for sys in [0, 1, 41, 59, 198, 257] {
            assert_action(&prog, sys, SECCOMP_RET_ALLOW);
        }
    }

    #[test]
    fn strict_kills_unexpected_arch() {
        let prog = build_strict();
        let got = run(&prog, 0xdead_beef, SYS_BPF, 0, 0);
        assert_eq!(got, SECCOMP_RET_KILL_PROCESS);
    }

    #[test]
    fn raw_socket_filter_still_denies_packet_raw() {
        let prog = build();
        // AF_PACKET (17) + SOCK_RAW (3) → deny.
        let got = run(&prog, AUDIT_ARCH, SYS_SOCKET, AF_PACKET, SOCK_RAW);
        assert_eq!(got, SECCOMP_RET_ERRNO | EPERM);
        // AF_INET + SOCK_RAW → deny.
        let got = run(&prog, AUDIT_ARCH, SYS_SOCKET, AF_INET, SOCK_RAW);
        assert_eq!(got, SECCOMP_RET_ERRNO | EPERM);
        // AF_INET + SOCK_DGRAM (2) → allow.
        let got = run(&prog, AUDIT_ARCH, SYS_SOCKET, AF_INET, 2);
        assert_eq!(got, SECCOMP_RET_ALLOW);
        // netlink (16) with a raw *type* (SOCK_RAW | CLOEXEC) → allow (the
        // filter keys on the domain first; agent network bootstrap uses it).
        let got = run(&prog, AUDIT_ARCH, SYS_SOCKET, 16, SOCK_RAW | 0x8000);
        assert_eq!(got, SECCOMP_RET_ALLOW);
        // A non-socket syscall → allow.
        let got = run(&prog, AUDIT_ARCH, 59, 0, 0);
        assert_eq!(got, SECCOMP_RET_ALLOW);
    }
}

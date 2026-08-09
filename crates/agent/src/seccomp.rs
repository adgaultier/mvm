//! seccomp-bpf policy for the guest agent.
//!
//! A seccomp filter installed by the agent is inherited by every process it
//! spawns, so this one filter hardens the whole guest: nothing under the
//! agent — the workload, exec sessions, or any of their children — may create
//! a raw packet or raw IP socket. Raw sockets are the classic privilege-
//! escalation surface when images or workloads are not fully trusted.
//!
//! The filter is deliberately narrow: it inspects only `socket(2)` and denies
//! only `AF_PACKET` (any type) plus `AF_INET`/`AF_INET6` with a raw type.
//! Everything else passes untouched, including the netlink sockets the
//! agent's own network bootstrap uses (`AF_NETLINK` with a raw *type* is
//! legitimate and stays allowed — the check keys on the domain first).
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

/// Install the raw-socket ban. Failure is fatal: a guest that cannot be
/// sandboxed will not be trusted to run.
pub fn install_raw_socket_filter() -> Result<(), std::io::Error> {
    let prog = build();
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

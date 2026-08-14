/* bpfprobe: probes the guest kernel's eBPF capabilities for Phase 2
 * (in-guest cgroup_skb egress policy via raw `bpf()` syscalls).
 *
 * Compile statically on the host (the guest is same-arch but has no
 * compiler):
 *
 *   cc -static -O2 -o bpfprobe bpfprobe.c
 *
 * `scripts/integration/sections/bpfprobe.sh` (via `just bpfprobe`) runs this
 * inside a VM through `mvm run ... -v`.
 * The workload runs as guest root (the whole point of the threat model),
 * so bpf() and the mounts below are permitted.
 *
 * Prints one `key=value` line per check:
 *   btf=1|0            /sys/kernel/btf/vmlinux present (needed for CO-RE/BPF-LSM)
 *   configgz=1|0       /proc/config.gz readable (CONFIG_* greppable)
 *   jit=<n>|na         /proc/sys/net/core/bpf_jit_enable
 *   cgroup2=<errno>    mount("cgroup2") result
 *   bpffs=<errno>      mount("bpf") result
 *   progl=<errno>      BPF_PROG_LOAD of a trivial cgroup_skb program
 *   attach=<errno>     BPF_PROG_ATTACH of that program to the cgroup
 *
 * errno 0 means the operation succeeded. The verdict line Phase 2 gates on
 * is `progl=0` AND `attach=0` (cgroup2 must also be usable).
 */
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/syscall.h>

/* ---- minimal eBPF uapi (self-contained; same across kernels) ---- */

#ifndef SYS_bpf
#ifdef __aarch64__
#define SYS_bpf 280
#else
#define SYS_bpf 321
#endif
#endif

/* bpf(2) commands */
#define BPF_MAP_CREATE 0
#define BPF_PROG_LOAD 5
#define BPF_PROG_ATTACH 8

/* program / attach types */
#define BPF_PROG_TYPE_CGROUP_SKB 28
#define BPF_CGROUP_INET_EGRESS 2

/* bpf_insn field encodings */
#define BPF_ALU64 0x07
#define BPF_MOV 0xb0
#define BPF_K 0x00
#define BPF_JMP 0x05
#define BPF_EXIT 0x90
#define BPF_REG_0 0

/* BPF_PROG_LOAD / BPF_PROG_ATTACH share one union; a zeroed 256-byte buffer
 * with the fields we need set covers both (unknown fields must be zero). */
union bpf_attr_probe {
    struct {
        uint32_t prog_type;         /* 0 */
        uint32_t insn_cnt;          /* 4 */
        uint64_t insns;             /* 8 */
        uint64_t license;           /* 16 */
        uint32_t log_level;         /* 24 */
        uint32_t log_size;          /* 28 */
        uint64_t log_buf;           /* 32 */
        uint32_t kern_version;      /* 40 */
        uint32_t prog_flags;        /* 44 */
        char prog_name[16];         /* 48 */
        uint32_t prog_ifindex;      /* 64 */
        uint32_t expected_attach_type; /* 68 */
    } load;
    struct {
        uint32_t target_fd;         /* 0 */
        uint32_t attach_bpf_fd;     /* 4 */
        uint32_t attach_type;       /* 8 */
        uint32_t attach_flags;      /* 12 */
    } attach;
    uint8_t raw[256];
};

struct bpf_insn_probe {
    uint8_t code;
    uint8_t dst_reg : 4;
    uint8_t src_reg : 4;
    int16_t off;
    int32_t imm;
};

static long bpf_call(int cmd, union bpf_attr_probe *attr) {
    return syscall(SYS_bpf, cmd, attr, sizeof(attr->raw));
}

static int file_exists(const char *path) {
    struct stat st;
    return stat(path, &st) == 0;
}

static int mount_errno(const char *type, const char *dir) {
    /* Best-effort; EBUSY (already mounted) is a success signal. */
    if (mkdir(dir, 0755) != 0 && errno != EEXIST)
        return errno;
    errno = 0;
    if (mount("mvmprobe", dir, type, 0, NULL) != 0)
        return errno;
    return 0;
}

int main(void) {
    static char logbuf[4096];

    printf("btf=%d\n", file_exists("/sys/kernel/btf/vmlinux"));
    printf("configgz=%d\n", file_exists("/proc/config.gz"));

    errno = 0;
    int jit = -1;
    FILE *jitf = fopen("/proc/sys/net/core/bpf_jit_enable", "r");
    if (jitf) {
        if (fscanf(jitf, "%d", &jit) != 1)
            jit = -1;
        fclose(jitf);
    }
    if (jit < 0)
        printf("jit=na\n");
    else
        printf("jit=%d\n", jit);

    printf("cgroup2=%d\n", mount_errno("cgroup2", "/tmp/mvmprobe-cgroup2"));
    printf("bpffs=%d\n", mount_errno("bpf", "/tmp/mvmprobe-bpffs"));

    /* Trivial cgroup_skb egress program: "return 1" (allow). Loading it
     * verifies the kernel accepts eBPF programs of this type at all. */
    static struct bpf_insn_probe prog[2];
    prog[0].code = BPF_ALU64 | BPF_MOV | BPF_K;
    prog[0].dst_reg = BPF_REG_0;
    prog[0].imm = 1;
    prog[1].code = BPF_JMP | BPF_EXIT;
    static const char license[] = "GPL";

    union bpf_attr_probe attr;
    memset(&attr, 0, sizeof(attr));
    attr.load.prog_type = BPF_PROG_TYPE_CGROUP_SKB;
    attr.load.insn_cnt = 2;
    attr.load.insns = (uint64_t)(uintptr_t)prog;
    attr.load.license = (uint64_t)(uintptr_t)license;
    attr.load.log_level = 1;
    attr.load.log_size = sizeof(logbuf);
    attr.load.log_buf = (uint64_t)(uintptr_t)logbuf;
    attr.load.expected_attach_type = BPF_CGROUP_INET_EGRESS;

    long prog_fd = bpf_call(BPF_PROG_LOAD, &attr);
    if (prog_fd < 0) {
        printf("progl=%d\n", errno);
        if (logbuf[0])
            printf("verifier=%s", logbuf);
        /* Attach is meaningless without a loaded program; emit the key so
         * the full key set is always present (integration.sh checks every
         * key is reported, whatever the verdict). */
        printf("attach=na\n");
        return 0;
    }
    printf("progl=0\n");

    int cg_fd = open("/tmp/mvmprobe-cgroup2", O_RDONLY | O_DIRECTORY);
    if (cg_fd < 0) {
        printf("attach=%d (no cgroup dir)\n", errno);
        close(prog_fd);
        return 0;
    }
    memset(&attr, 0, sizeof(attr));
    attr.attach.target_fd = (uint32_t)cg_fd;
    attr.attach.attach_bpf_fd = (uint32_t)prog_fd;
    attr.attach.attach_type = BPF_CGROUP_INET_EGRESS;
    long rc = bpf_call(BPF_PROG_ATTACH, &attr);
    printf("attach=%d\n", rc == 0 ? 0 : errno);
    close(cg_fd);
    close(prog_fd);
    return 0;
}

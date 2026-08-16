/* strictprobe: verify the workload-scoped strict seccomp filter.
 *
 * Each syscall is attempted with deliberately invalid arguments. Without the
 * filter the kernel normally returns another error (or, for unshare(0),
 * succeeds); with --security=strict seccomp must intercept it with EPERM.
 */
#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifdef __aarch64__
#define S_PTRACE 117
#define S_MOUNT 40
#define S_UMOUNT2 39
#define S_PIVOT_ROOT 41
#define S_UNSHARE 97
#define S_SETNS 268
#define S_INIT_MODULE 105
#define S_DELETE_MODULE 106
#define S_FINIT_MODULE 273
#define S_KEXEC_LOAD 104
#define S_KEXEC_FILE_LOAD 294
#else
#define S_PTRACE 101
#define S_MOUNT 165
#define S_UMOUNT2 166
#define S_PIVOT_ROOT 155
#define S_UNSHARE 272
#define S_SETNS 308
#define S_INIT_MODULE 175
#define S_DELETE_MODULE 176
#define S_FINIT_MODULE 313
#define S_KEXEC_LOAD 246
#define S_KEXEC_FILE_LOAD 320
#endif

#define S_PRCTL 157
#ifdef __aarch64__
#undef S_PRCTL
#define S_PRCTL 167
#endif
#define PR_GET_NO_NEW_PRIVS 39

#define CHECK(name, call) \
    do { \
        errno = 0; \
        (void)(call); \
        printf(name "=%d\n", errno == EPERM); \
        fflush(stdout); \
    } while (0)

int main(void) {
    errno = 0;
    printf("no_new_privs=%d\n",
           syscall(S_PRCTL, PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) == 1);
    fflush(stdout);

    CHECK("ptrace", syscall(S_PTRACE, 0, 0, 0, 0));
    CHECK("mount", syscall(S_MOUNT, 0, 0, 0, 0, 0));
    CHECK("umount2", syscall(S_UMOUNT2, 0, 0));
    CHECK("pivot_root", syscall(S_PIVOT_ROOT, 0, 0));
    CHECK("unshare", syscall(S_UNSHARE, 0));
    CHECK("setns", syscall(S_SETNS, -1, 0));
    CHECK("init_module", syscall(S_INIT_MODULE, 0, 0, 0));
    CHECK("delete_module", syscall(S_DELETE_MODULE, 0, 0));
    CHECK("finit_module", syscall(S_FINIT_MODULE, -1, 0, 0));
    CHECK("kexec_load", syscall(S_KEXEC_LOAD, 0, 0, 0, 0, 0));
    CHECK("kexec_file_load", syscall(S_KEXEC_FILE_LOAD, -1, -1, 0, 0, 0));
    return 0;
}

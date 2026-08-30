/* chownprobe: verify CAP_CHOWN presence in the workload.
 *
 * Attempts chown() as root on a file in the writable /probe mount. Without
 * strict mode the workload keeps CAP_CHOWN, so root chown succeeds
 * (chown=0). With --security=strict the guestd drops CAP_CHOWN from the
 * bounding set, so chown fails with EPERM even as root (chown=1). This is a
 * capability check, not a seccomp denial — the strict seccomp filter does not
 * touch the chown syscalls.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>

#define CHECK(name, call) \
    do { \
        errno = 0; \
        (void)(call); \
        printf(name "=%d\n", errno == EPERM); \
        fflush(stdout); \
    } while (0)

int main(void) {
    int fd = open("/probe/chownprobe.t", O_CREAT | O_WRONLY | O_TRUNC, 0600);
    if (fd >= 0) close(fd);
    /* Re-owning to a different uid always needs CAP_CHOWN, independent of the
     * file's current owner, so the result is unambiguous under both modes. */
    CHECK("chown", chown("/probe/chownprobe.t", 1234, 1234));
    return 0;
}

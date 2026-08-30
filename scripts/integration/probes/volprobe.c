/* volprobe: probes virtio-fs mount permission semantics (LinuxSimplified).
 *
 * Prints the requesting identity, then attempts to stat and write the mounted
 * directory's backing file. Run under different guest uids (root / -u N) to
 * observe whether the DAC check is enforced guest-side on the stat-reported
 * owner, or bypassed. The actual file is /data/f within the mounted share.
 *
 *   cc -static -O2 -o volprobe volprobe.c
 */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

int main(void) {
    printf("uid=%d gid=%d\n", (int)getuid(), (int)getgid());

    struct stat st;
    if (stat("/data/f", &st) == 0) {
        printf("stat_owner=%d:%d stat_mode=%o\n", (int)st.st_uid,
               (int)st.st_gid, st.st_mode & 07777);
    } else {
        printf("stat_errno=%d\n", errno);
    }

    int fd = open("/data/nonroot.txt", O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (fd < 0) {
        printf("create_errno=%d\n", errno);
    } else {
        const char *msg = "content\n";
        ssize_t n = write(fd, msg, strlen(msg));
        int we = errno;
        printf("write_bytes=%zd write_errno=%d\n", n, we);
        close(fd);
    }
    return 0;
}

/* rawprobe: probes the guest's seccomp raw-socket ban.
 *
 * Prints one `key=errno` line per socket(2) attempt; errno 0 means the call
 * succeeded. Compile statically on the host (the guest is same-arch but has
 * no compiler):
 *
 *   cc -static -O2 -o rawprobe rawprobe.c
 *
 * scripts/integration.sh runs this inside a VM via `mvm run ... -v`.
 */
#include <errno.h>
#include <stdio.h>
#include <unistd.h>

#include <sys/socket.h>
#include <linux/netlink.h>

static int probe(int domain, int type) {
    errno = 0;
    int fd = socket(domain, type, 0);
    int e = errno;
    if (fd >= 0)
        close(fd);
    return e;
}

int main(void) {
    printf("packet_raw=%d\n", probe(AF_PACKET, SOCK_RAW));
    printf("inet_raw=%d\n", probe(AF_INET, SOCK_RAW));
    printf("inet6_raw=%d\n", probe(AF_INET6, SOCK_RAW));
    printf("inet_raw_nonblock=%d\n", probe(AF_INET, SOCK_RAW | SOCK_NONBLOCK));
    printf("inet_raw_cloexec=%d\n", probe(AF_INET, SOCK_RAW | SOCK_CLOEXEC));
    printf("inet_dgram=%d\n", probe(AF_INET, SOCK_DGRAM));
    printf("netlink_dgram=%d\n", probe(AF_NETLINK, SOCK_DGRAM));
    return 0;
}

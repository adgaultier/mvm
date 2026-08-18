/* vsockprobe: a minimal Agent API client for the guest side of
 * `doc/agentic/notifications-delegation.md` — the Agent API rides a
 * per-sandbox vsock channel (guest -> CID 2, port 24643), not HTTP, so it
 * can only be exercised from inside the guest. Alpine ships no python3/cc,
 * so this is compiled statically on the host and mounted in, same as
 * bpfprobe.c/rawprobe.c/strictprobe.c.
 *
 * Compile statically on the host (the guest is same-arch but has no
 * compiler):
 *
 *   cc -static -O2 -o vsockprobe vsockprobe.c
 *
 * `scripts/integration/sections/agent-api.sh` (via `just agent-api`) runs
 * this inside a VM through `mvm exec`.
 *
 * Usage: vsockprobe <token> <method> [params-json]
 *
 * Sends one length-prefixed JSON request (see `mvm_common::protocol::
 * encode_frame` / `mvm_common::api::AgentApiRequest`) over AF_VSOCK to
 * (VMADDR_CID_HOST, AGENT_API_VSOCK_PORT) and prints the raw JSON response
 * to stdout. Exit code is 0 iff the round trip itself succeeded (connect,
 * send, receive a well-formed frame) — an application-level `"ok":false` in
 * the response is not a probe failure, it's a normal Agent API error.
 *
 * The sockaddr_vm layout is self-defined rather than pulled from
 * <linux/vm_sockets.h>: that header is kernel-headers, not libc, and may not
 * be present wherever this gets cross-compiled (same rationale as the
 * self-contained eBPF uapi in bpfprobe.c). It is stable ABI, unchanged since
 * vsock's introduction.
 */
#include <arpa/inet.h>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/socket.h>

#ifndef AF_VSOCK
#define AF_VSOCK 40
#endif

#define VMADDR_CID_HOST 2
#define AGENT_API_VSOCK_PORT 24643

struct sockaddr_vm {
    uint16_t svm_family;
    uint16_t svm_reserved1;
    uint32_t svm_port;
    uint32_t svm_cid;
    uint8_t svm_zero[4];
};

static int send_all(int fd, const void *buf, size_t len) {
    const uint8_t *p = buf;
    while (len > 0) {
        ssize_t n = write(fd, p, len);
        if (n <= 0)
            return -1;
        p += n;
        len -= (size_t)n;
    }
    return 0;
}

static int recv_all(int fd, void *buf, size_t len) {
    uint8_t *p = buf;
    while (len > 0) {
        ssize_t n = read(fd, p, len);
        if (n <= 0)
            return -1;
        p += n;
        len -= (size_t)n;
    }
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: %s <token> <method> [params-json]\n", argv[0]);
        return 2;
    }
    const char *token = argv[1];
    const char *method = argv[2];
    const char *params = argc > 3 ? argv[3] : "{}";

    char req[8192];
    int req_len = snprintf(req, sizeof(req), "{\"method\":\"%s\",\"token\":\"%s\",\"params\":%s}",
                            method, token, params);
    if (req_len < 0 || (size_t)req_len >= sizeof(req)) {
        fprintf(stderr, "vsockprobe: request too large\n");
        return 2;
    }

    int fd = socket(AF_VSOCK, SOCK_STREAM, 0);
    if (fd < 0) {
        fprintf(stderr, "vsockprobe: socket: %s\n", strerror(errno));
        return 1;
    }

    struct sockaddr_vm addr = {0};
    addr.svm_family = AF_VSOCK;
    addr.svm_port = AGENT_API_VSOCK_PORT;
    addr.svm_cid = VMADDR_CID_HOST;
    if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
        fprintf(stderr, "vsockprobe: connect: %s\n", strerror(errno));
        close(fd);
        return 1;
    }

    uint32_t len_be = htonl((uint32_t)req_len);
    if (send_all(fd, &len_be, sizeof(len_be)) != 0 || send_all(fd, req, (size_t)req_len) != 0) {
        fprintf(stderr, "vsockprobe: send: %s\n", strerror(errno));
        close(fd);
        return 1;
    }

    if (recv_all(fd, &len_be, sizeof(len_be)) != 0) {
        fprintf(stderr, "vsockprobe: recv header: %s\n", strerror(errno));
        close(fd);
        return 1;
    }
    uint32_t resp_len = ntohl(len_be);
    if (resp_len == 0 || resp_len > sizeof(req) * 4) {
        fprintf(stderr, "vsockprobe: implausible response length %u\n", resp_len);
        close(fd);
        return 1;
    }

    char *resp = malloc(resp_len + 1);
    if (!resp) {
        close(fd);
        return 1;
    }
    if (recv_all(fd, resp, resp_len) != 0) {
        fprintf(stderr, "vsockprobe: recv body: %s\n", strerror(errno));
        free(resp);
        close(fd);
        return 1;
    }
    resp[resp_len] = '\0';
    printf("%s\n", resp);
    free(resp);
    close(fd);
    return 0;
}

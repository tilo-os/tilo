/* One filesystem, socket or descriptor operation per run; the exit code is
 * the result: 0 succeeded, 1 was refused. Kept in C so the probe links only
 * libSystem and compiles with the `cc` every Rust toolchain on macOS already
 * requires. The marker string gives the tamper test a byte to flip. */
#include <arpa/inet.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

static const char marker[] = "TILO_SEATBELT_PROBE_MARKER";

int64_t sandbox_extension_consume(const char *token);

int main(int argc, char **argv) {
    if (argc != 3) {
        return 2;
    }
    const char *op = argv[1];
    const char *arg = argv[2];
    if (strcmp(op, "read") == 0) {
        char byte;
        int fd = open(arg, O_RDONLY);
        return fd >= 0 && read(fd, &byte, 1) >= 0 ? 0 : 1;
    }
    if (strcmp(op, "write") == 0) {
        int fd = open(arg, O_WRONLY | O_CREAT | O_TRUNC, 0600);
        return fd >= 0 && write(fd, marker, sizeof marker) == sizeof marker ? 0 : 1;
    }
    if (strcmp(op, "picked") == 0) {
        /* Consume $TILO_PROBE_TOKEN, if any, then open arg for writing. */
        const char *token = getenv("TILO_PROBE_TOKEN");
        if (token != NULL && sandbox_extension_consume(token) < 0) {
            return 3;
        }
        int fd = open(arg, O_RDWR);
        return fd >= 0 && write(fd, marker, sizeof marker) == sizeof marker ? 0 : 1;
    }
    if (strcmp(op, "replace") == 0) {
        /* Consume $TILO_PROBE_TOKENS (one per line), save arg by writing
         * its sibling ".<name>.tilo-save" and renaming that over it, then
         * try to create any other file beside it (exit 5 if that worked). */
        const char *tokens = getenv("TILO_PROBE_TOKENS");
        char buffer[4096];
        if (tokens == NULL || strlen(tokens) >= sizeof buffer) {
            return 2;
        }
        strcpy(buffer, tokens);
        for (char *token = strtok(buffer, "\n"); token; token = strtok(NULL, "\n")) {
            if (sandbox_extension_consume(token) < 0) {
                return 3;
            }
        }
        const char *slash = strrchr(arg, '/');
        if (slash == NULL) {
            return 2;
        }
        char sibling[2048], other[2048];
        int dir = (int)(slash - arg);
        snprintf(sibling, sizeof sibling, "%.*s/.%s.tilo-save", dir, arg, slash + 1);
        snprintf(other, sizeof other, "%.*s/unrelated.tmp", dir, arg);
        int fd = open(sibling, O_WRONLY | O_CREAT | O_TRUNC, 0644);
        if (fd < 0 || write(fd, marker, sizeof marker) != sizeof marker || close(fd) != 0) {
            return 1;
        }
        if (rename(sibling, arg) != 0) {
            return 1;
        }
        return open(other, O_WRONLY | O_CREAT, 0644) >= 0 ? 5 : 0;
    }
    if (strcmp(op, "capture") == 0) {
        /* Ask the capture broker on $TILO_SVC_CAPTURE_FD for arg ("camera
         * W H"): send one end of a fresh socket pair with the request, then
         * expect a grant and one picture on it. Exit 1 when refused. */
        const char *control = getenv("TILO_SVC_CAPTURE_FD");
        int pair[2];
        if (control == NULL || socketpair(AF_UNIX, SOCK_STREAM, 0, pair) != 0) {
            return 2;
        }
        char space[CMSG_SPACE(sizeof(int))];
        memset(space, 0, sizeof space);
        struct iovec iov = {(void *)arg, strlen(arg)};
        struct msghdr msg;
        memset(&msg, 0, sizeof msg);
        msg.msg_iov = &iov;
        msg.msg_iovlen = 1;
        msg.msg_control = space;
        msg.msg_controllen = sizeof space;
        struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
        cmsg->cmsg_level = SOL_SOCKET;
        cmsg->cmsg_type = SCM_RIGHTS;
        cmsg->cmsg_len = CMSG_LEN(sizeof(int));
        memcpy(CMSG_DATA(cmsg), &pair[1], sizeof(int));
        if (sendmsg(atoi(control), &msg, 0) < 0) {
            return 2;
        }
        close(pair[1]);
        for (int expected = 1; expected <= 3; expected += 2) {
            unsigned char head[13];
            size_t got = 0;
            while (got < sizeof head) {
                ssize_t n = read(pair[0], head + got, sizeof head - got);
                if (n <= 0) {
                    return 2;
                }
                got += (size_t)n;
            }
            if (head[0] == 2) {
                return 1;
            }
            if (head[0] != expected) {
                return 2;
            }
            unsigned len = head[9] | head[10] << 8 | head[11] << 16 | (unsigned)head[12] << 24;
            for (char sink[4096]; len > 0;) {
                ssize_t n = read(pair[0], sink, len < sizeof sink ? len : sizeof sink);
                if (n <= 0) {
                    return 2;
                }
                len -= (unsigned)n;
            }
        }
        return 0;
    }
    if (strcmp(op, "exec") == 0) {
        execl(arg, arg, (char *)NULL);
        return 1;
    }
    if (strcmp(op, "fd") == 0) {
        return write(atoi(arg), "ok", 2) == 2 ? 0 : 1;
    }
    if (strcmp(op, "tcp") == 0) {
        struct sockaddr_in addr;
        memset(&addr, 0, sizeof addr);
        addr.sin_family = AF_INET;
        addr.sin_port = htons((unsigned short)atoi(arg));
        addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        return fd >= 0 && connect(fd, (struct sockaddr *)&addr, sizeof addr) == 0 ? 0 : 1;
    }
    if (strcmp(op, "proxy") == 0) {
        /* CONNECT to arg (host:port) through $HTTPS_PROXY, as an ordinary
         * HTTP client would. */
        const char *proxy = getenv("HTTPS_PROXY");
        const char *prefix = "http://127.0.0.1:";
        if (proxy == NULL || strncmp(proxy, prefix, strlen(prefix)) != 0) {
            return 1;
        }
        struct sockaddr_in addr;
        memset(&addr, 0, sizeof addr);
        addr.sin_family = AF_INET;
        addr.sin_port = htons((unsigned short)atoi(proxy + strlen(prefix)));
        addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        if (fd < 0 || connect(fd, (struct sockaddr *)&addr, sizeof addr) != 0) {
            return 1;
        }
        char request[512];
        int len = snprintf(request, sizeof request, "CONNECT %s HTTP/1.1\r\n\r\n", arg);
        if (len <= 0 || write(fd, request, (size_t)len) != len) {
            return 1;
        }
        char reply[13] = {0};
        return read(fd, reply, 12) == 12 && strcmp(reply, "HTTP/1.1 200") == 0 ? 0 : 1;
    }
    if (strcmp(op, "unix") == 0) {
        struct sockaddr_un addr;
        memset(&addr, 0, sizeof addr);
        addr.sun_family = AF_UNIX;
        strncpy(addr.sun_path, arg, sizeof addr.sun_path - 1);
        int fd = socket(AF_UNIX, SOCK_STREAM, 0);
        return fd >= 0 && connect(fd, (struct sockaddr *)&addr, sizeof addr) == 0 ? 0 : 1;
    }
    if (strcmp(op, "cache") == 0) {
        /* Write arg below the cache directory frameworks such as Metal use. */
        char dir[1024];
        size_t len = confstr(_CS_DARWIN_USER_CACHE_DIR, dir, sizeof dir);
        if (len == 0 || len > sizeof dir) {
            return 1;
        }
        char path[2048];
        snprintf(path, sizeof path, "%s%s", dir, arg);
        int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
        return fd >= 0 && write(fd, marker, sizeof marker) == sizeof marker ? 0 : 1;
    }
    if (strcmp(op, "env") == 0) {
        /* arg is NAME=expected */
        const char *eq = strchr(arg, '=');
        if (eq == NULL) {
            return 2;
        }
        char name[256];
        size_t len = (size_t)(eq - arg);
        if (len >= sizeof name) {
            return 2;
        }
        memcpy(name, arg, len);
        name[len] = '\0';
        const char *value = getenv(name);
        return value != NULL && strcmp(value, eq + 1) == 0 ? 0 : 1;
    }
    return 2;
}

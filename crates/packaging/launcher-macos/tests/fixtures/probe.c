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

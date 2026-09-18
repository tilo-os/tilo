/* Linux CPU-list text must not make libc CPU counts depend on stale stack bytes. */
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

__attribute__((noinline)) static void seed_stack(void) {
    volatile char bytes[4096];
    for (unsigned int i = 0; i < sizeof(bytes); ++i) bytes[i] = i % 2 ? 'x' : '8';
    __asm__ volatile ("" : : "r"(bytes) : "memory");
}

static long count_cpus(const char *text) {
    unsigned long count = 0;
    const char *p = text;
    while (*p && *p != '\n') {
        if (*p < '0' || *p > '9') return -1;
        char *end;
        unsigned long first = strtoul(p, &end, 10), last = first;
        if (*end == '-') {
            p = end + 1;
            if (*p < '0' || *p > '9') return -1;
            last = strtoul(p, &end, 10);
        }
        if (last < first || last > INT_MAX) return -1;
        unsigned long span = last - first + 1;
        if (span > INT_MAX || count > INT_MAX - span) return -1;
        count += span;
        if (*end == ',') {
            p = end + 1;
            if (!*p || *p == '\n') return -1;
        } else if (!*end || (*end == '\n' && !end[1])) {
            return (long)count;
        } else {
            return -1;
        }
    }
    return -1;
}

int main(void) {
    const char *paths[] = {
        "/sys/devices/system/cpu/online", "/sys/devices/system/cpu/present",
    };
    int failed = 0;
    for (unsigned int i = 0; i < 2; ++i) {
        char bytes[4096];
        int fd = open(paths[i], O_RDONLY);
        if (fd < 0) { perror(paths[i]); return 1; }
        ssize_t length = read(fd, bytes, sizeof(bytes) - 1);
        close(fd);
        if (length <= 0 || length == sizeof(bytes) - 1) {
            fprintf(stderr, "Invalid or truncated CPU list: %s\n", paths[i]);
            return 1;
        }
        bytes[length] = 0; /* Parse our copy safely, independently of libc's reader. */
        long expected = count_cpus(bytes);
        int newline = bytes[length - 1] == '\n';
        printf("%s: bytes=%zd newline=%d count=%ld\n", paths[i], length, newline, expected);
        if (!newline || expected <= 0) failed = 1;
        if (i == 0) {
            seed_stack();
            long online = sysconf(_SC_NPROCESSORS_ONLN);
            printf("Seeded sysconf online=%ld, expected=%ld\n", online, expected);
            if (online != expected) failed = 1;
        }
    }
    if (failed) {
        fputs("CPU-list ABI contract failed\n", stderr);
        return 1;
    }
    puts("TILO_CPU_LIST_PASS");
    return 0;
}

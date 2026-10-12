// Experimental public-model allocator trace, never linked into production.
// Log only allocation sizes, process limits and memory counters; no data bytes.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <unistd.h>

typedef int (*aligned_function)(void **, size_t, size_t);
static aligned_function real_aligned;
// This review fixture is explicitly glibc-only. Direct libc entrypoints avoid
// recursive dlsym allocation during loader initialization; no production ABI.
extern void *__libc_malloc(size_t);
extern void *__libc_calloc(size_t, size_t);

__attribute__((constructor)) static void initialize(void) {
    real_aligned = (aligned_function)dlsym(RTLD_NEXT, "posix_memalign");
    if (!real_aligned) _exit(125);
}

static void observe(const char *kind, const char *phase, size_t size, int result) {
    char status[16384], record[512];
    unsigned long long virtual_kib = 0, resident_kib = 0;
    int fd = open("/proc/self/status", O_RDONLY | O_CLOEXEC);
    if (fd < 0) _exit(125);
    ssize_t count = read(fd, status, sizeof(status) - 1);
    close(fd);
    if (count <= 0) _exit(125);
    status[count] = 0;
    const char *virt = strstr(status, "VmSize:");
    const char *rss = strstr(status, "VmRSS:");
    if (!virt || !rss) _exit(125);
    virtual_kib = strtoull(virt + 7, NULL, 10);
    resident_kib = strtoull(rss + 6, NULL, 10);
    struct rlimit limit;
    if (getrlimit(RLIMIT_AS, &limit) != 0) _exit(125);
    int bytes = snprintf(record, sizeof(record),
        "ASR_ALLOCATION {\"kind\":\"%s\",\"phase\":\"%s\",\"requested_bytes\":%zu,"
        "\"result\":%d,\"virtual_kib\":%llu,\"resident_kib\":%llu,"
        "\"address_space_limit_bytes\":%llu}\n",
        kind, phase, size, result, virtual_kib, resident_kib,
        (unsigned long long)limit.rlim_cur);
    if (bytes <= 0 || bytes >= (int)sizeof(record)
            || write(STDERR_FILENO, record, (size_t)bytes) != bytes) _exit(125);
}

int posix_memalign(void **output, size_t alignment, size_t size) {
    if (!real_aligned) _exit(125);
    const int traced = size >= 16 * 1024 * 1024;
    if (traced) observe("aligned", "before", size, 0);
    int result = real_aligned(output, alignment, size);
    if (traced) observe("aligned", "after", size, result);
    return result;
}

void *malloc(size_t size) {
    const int traced = size >= 16 * 1024 * 1024;
    if (traced) observe("malloc", "before", size, 0);
    void *result = __libc_malloc(size);
    int saved_errno = errno;
    if (traced) observe("malloc", "after", size, result ? 0 : saved_errno);
    errno = saved_errno;
    return result;
}

void *calloc(size_t count, size_t size) {
    const int overflow = size && count > (size_t)-1 / size;
    const size_t total = overflow ? (size_t)-1 : count * size;
    const int traced = total >= 16 * 1024 * 1024;
    if (traced) observe("calloc", "before", total, 0);
    void *result = __libc_calloc(count, size);
    int saved_errno = errno;
    if (traced) observe("calloc", "after", total, result ? 0 : saved_errno);
    errno = saved_errno;
    return result;
}

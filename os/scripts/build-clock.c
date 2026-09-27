// Fix only the wall clock seen by offline DNF transactions during image builds.
// RPM writes installtime/installtid into its database, and DNF writes transaction
// times into its history. BuildKit's SOURCE_DATE_EPOCH only fixes layer mtimes.
#define _GNU_SOURCE
#include <errno.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

static time_t image_epoch(void) {
    const char *value = getenv("SOURCE_DATE_EPOCH");
    if (!value || !*value) {
        _exit(125);
    }
    char *end = NULL;
    errno = 0;
    long long parsed = strtoll(value, &end, 10);
    if (errno || !end || *end || parsed < 0 || (time_t)parsed != parsed) {
        _exit(125);
    }
    return (time_t)parsed;
}

time_t time(time_t *result) {
    time_t fixed = image_epoch();
    if (result) *result = fixed;
    return fixed;
}

int gettimeofday(struct timeval *result, void *zone) {
    (void)zone;
    result->tv_sec = image_epoch();
    result->tv_usec = 0;
    return 0;
}

int clock_gettime(clockid_t clock, struct timespec *result) {
    if (clock == CLOCK_REALTIME || clock == CLOCK_REALTIME_COARSE) {
        result->tv_sec = image_epoch();
        result->tv_nsec = 0;
        return 0;
    }
    return (int)syscall(SYS_clock_gettime, clock, result);
}

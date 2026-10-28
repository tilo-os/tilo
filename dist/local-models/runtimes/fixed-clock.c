/* A clock that only ticks forward by a microsecond each time it is read, for nvcc's compilers
 * (llama-cpp-cuda.Containerfile). cicc's output depends on the time it reads while compiling the
 * larger kernels, so the same source compiled twice gave different PTX; with this clock (and
 * without address randomization) it gives the same PTX every time. */
#define _GNU_SOURCE
#include <string.h>
#include <sys/resource.h>
#include <sys/time.h>
#include <sys/times.h>
#include <time.h>

static unsigned long long ticks;

static unsigned long long tick(void) { return __atomic_add_fetch(&ticks, 1000, __ATOMIC_RELAXED); }

int clock_gettime(clockid_t clock, struct timespec *t) {
  (void)clock;
  unsigned long long n = tick();
  t->tv_sec = (time_t)(n / 1000000000);
  t->tv_nsec = (long)(n % 1000000000);
  return 0;
}

int gettimeofday(struct timeval *t, void *zone) {
  (void)zone;
  unsigned long long n = tick();
  if (t) {
    t->tv_sec = (time_t)(n / 1000000000);
    t->tv_usec = (suseconds_t)(n % 1000000000 / 1000);
  }
  return 0;
}

time_t time(time_t *t) {
  time_t now = (time_t)(tick() / 1000000000);
  if (t) *t = now;
  return now;
}

clock_t clock(void) { return (clock_t)(tick() / 1000); }

clock_t times(struct tms *t) {
  clock_t now = (clock_t)(tick() / 10000000);
  if (t) {
    memset(t, 0, sizeof *t);
    t->tms_utime = now;
  }
  return now;
}

int getrusage(int who, struct rusage *u) {
  (void)who;
  unsigned long long n = tick();
  memset(u, 0, sizeof *u);
  u->ru_utime.tv_sec = (time_t)(n / 1000000000);
  u->ru_utime.tv_usec = (suseconds_t)(n % 1000000000 / 1000);
  return 0;
}

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <sys/resource.h>
#include <sys/types.h>
#include <unistd.h>
#ifdef __linux__
#include <grp.h>
#include <limits.h>
#include <linux/audit.h>
#include <linux/capability.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <stddef.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#define ALLOW(n)                                                               \
  BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_##n, 0, 1),                         \
      BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW)
#define FD_ONLY(n, fd)                                                         \
  BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_##n, 0, 4),                         \
      BPF_STMT(BPF_LD | BPF_W | BPF_ABS,                                       \
               offsetof(struct seccomp_data, args[0])),                        \
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, fd, 0, 1),                           \
      BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),                            \
      BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS)
static int sandbox(void) {
#if defined(__x86_64__)
#define QOS_ARCH AUDIT_ARCH_X86_64
#elif defined(__aarch64__)
#define QOS_ARCH AUDIT_ARCH_AARCH64
#else
#error Unsupported seccomp architecture
#endif
  struct sock_filter filter[] = {
      BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, QOS_ARCH, 1, 0),
      BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
      BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
      FD_ONLY(pread64, 3), FD_ONLY(write, 4), FD_ONLY(fstat, 3),
      /* fcntl may only inspect F_GETFL on the inherited readonly descriptor. */
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_fcntl, 0, 7),
      BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
               offsetof(struct seccomp_data, args[0])),
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, 3, 0, 4),
      BPF_STMT(BPF_LD | BPF_W | BPF_ABS,
               offsetof(struct seccomp_data, args[1])),
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, F_GETFL, 0, 2),
      BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
      BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
      BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS), ALLOW(exit),
      ALLOW(exit_group), ALLOW(brk), ALLOW(mmap), ALLOW(munmap),
      ALLOW(mprotect), ALLOW(madvise), ALLOW(mremap), ALLOW(rt_sigreturn),
      ALLOW(rt_sigprocmask), ALLOW(rt_sigaction), ALLOW(futex),
      ALLOW(clock_gettime), ALLOW(sched_yield), ALLOW(close),
      BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS)};
  struct sock_fprog program = {
      (unsigned short)(sizeof filter / sizeof filter[0]), filter};
  return prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &program);
}
#endif
int qos_child_prepare(int input, int output, uint32_t uid, uint32_t gid,
                      int production, int expected_parent) {
  int in_copy = fcntl(input, F_DUPFD_CLOEXEC, 10),
      out_copy = fcntl(output, F_DUPFD_CLOEXEC, 10);
  if (in_copy < 0 || out_copy < 0 || dup2(in_copy, 3) < 0 ||
      dup2(out_copy, 4) < 0)
    return -1;
  close(0);
  close(1);
  close(2);
#ifdef __linux__
  if (syscall(SYS_close_range, 5, UINT_MAX, 0))
    return -1;
#else
  struct rlimit inherited;
  if (getrlimit(RLIMIT_NOFILE, &inherited) || inherited.rlim_cur > 1048576)
    return -1;
  for (int fd = 5; (rlim_t)fd < inherited.rlim_cur; ++fd)
    close(fd);
#endif
  struct rlimit cpu = {10, 10}, zero = {0, 0}, files = {5, 5};
#ifdef __linux__
  struct rlimit memory = {256 * 1024 * 1024, 256 * 1024 * 1024};
  if (setrlimit(RLIMIT_AS, &memory))
    return -11;
#endif
  if (setrlimit(RLIMIT_CPU, &cpu))
    return -12;
  if (setrlimit(RLIMIT_CORE, &zero))
    return -13;
  if (setrlimit(RLIMIT_FSIZE, &zero))
    return -14;
  if (setrlimit(RLIMIT_NOFILE, &files))
    return -15;
#ifdef __linux__
  if (production) {
    if (!uid || !gid)
      return -1;
    for (int c = 0; c < 64; ++c)
      if (prctl(PR_CAPBSET_DROP, c, 0, 0, 0) && errno != EINVAL)
        return -1;
    if (setgroups(0, NULL) || setresgid(gid, gid, gid) ||
        setresuid(uid, uid, uid))
      return -1;
    struct __user_cap_header_struct h = {_LINUX_CAPABILITY_VERSION_3, 0};
    struct __user_cap_data_struct data[2] = {{0}, {0}};
    if (syscall(SYS_capset, &h, data))
      return -1;
  }
  if (prctl(PR_SET_PDEATHSIG, SIGKILL, 0, 0, 0) || getppid() != expected_parent)
    return -16;
  if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) || sandbox())
    return -1;
#else
  (void)expected_parent;
  (void)uid;
  (void)gid;
  if (production)
    return -1;
#endif
  return 0;
}

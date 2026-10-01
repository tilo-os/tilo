#define _GNU_SOURCE
#include <asm/ptrace.h>
#include <elf.h>
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

/* Disposable native ARM guest diagnostic. Trace new threads so a render
 * worker's SIGILL has its own PC even if a library changes signal handlers. */
static void report_sigill(pid_t tid) {
    struct user_pt_regs regs = {0};
    struct iovec io = {.iov_base = &regs, .iov_len = sizeof(regs)};
    if (ptrace(PTRACE_GETREGSET, tid, (void *)NT_PRSTATUS, &io) == -1) {
        dprintf(STDERR_FILENO, "TILO_SIGILL tid=%ld register_error=%s\n",
                (long)tid, strerror(errno));
        return;
    }
    siginfo_t info = {0};
    ptrace(PTRACE_GETSIGINFO, tid, NULL, &info);
    errno = 0;
    long instruction = ptrace(PTRACE_PEEKDATA, tid, (void *)regs.pc, NULL);
    dprintf(STDERR_FILENO,
            "TILO_SIGILL tid=%ld pc=0x%llx sp=0x%llx si_code=%d instruction=0x%08lx peek_errno=%d\n",
            (long)tid, (unsigned long long)regs.pc,
            (unsigned long long)regs.sp, info.si_code,
            (unsigned long)instruction & 0xffffffffUL, errno);

    char maps_path[64];
    snprintf(maps_path, sizeof(maps_path), "/proc/%ld/maps", (long)tid);
    FILE *maps = fopen(maps_path, "r");
    if (!maps) return;
    char line[1024];
    while (fgets(line, sizeof(line), maps)) {
        unsigned long long start, end;
        if (sscanf(line, "%llx-%llx", &start, &end) == 2 &&
            regs.pc >= start && regs.pc < end) {
            dprintf(STDERR_FILENO, "TILO_SIGILL mapping=%s", line);
            break;
        }
    }
    fclose(maps);
}

int main(int argc, char **argv) {
    if (argc < 2) return 2;
    pid_t child = fork();
    if (child == -1) return 2;
    if (child == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) == -1) _exit(127);
        raise(SIGSTOP);
        execv(argv[1], &argv[1]);
        _exit(127);
    }

    int status;
    if (waitpid(child, &status, __WALL) != child || !WIFSTOPPED(status)) return 2;
    if (ptrace(PTRACE_SETOPTIONS, child, NULL,
               (void *)(uintptr_t)(PTRACE_O_TRACECLONE | PTRACE_O_EXITKILL)) == -1) {
        dprintf(STDERR_FILENO, "TILO_PTRACE setup_error=%s\n", strerror(errno));
        kill(child, SIGKILL);
        return 2;
    }
    dprintf(STDERR_FILENO, "TILO_PTRACE_STARTED pid=%ld\n", (long)child);
    ptrace(PTRACE_CONT, child, NULL, NULL);

    int result = 2;
    pid_t tid;
    while ((tid = waitpid(-1, &status, __WALL)) > 0) {
        if (WIFEXITED(status)) {
            if (tid == child) result = WEXITSTATUS(status);
            continue;
        }
        if (WIFSIGNALED(status)) {
            if (tid == child) result = 128 + WTERMSIG(status);
            continue;
        }
        if (!WIFSTOPPED(status)) continue;
        int signo = WSTOPSIG(status);
        if (signo == SIGILL) report_sigill(tid);
        int deliver = (signo == SIGTRAP || signo == SIGSTOP) ? 0 : signo;
        if (ptrace(PTRACE_CONT, tid, NULL, (void *)(uintptr_t)deliver) == -1)
            dprintf(STDERR_FILENO, "TILO_PTRACE continue_error tid=%ld: %s\n",
                    (long)tid, strerror(errno));
    }
    return result;
}

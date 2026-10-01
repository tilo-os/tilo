#define _GNU_SOURCE
#include <dlfcn.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <ucontext.h>
#include <unistd.h>

/* Diagnostic only: loaded into the disposable KVM guest greeter by LD_PRELOAD. */
static void on_sigill(int signal_number, siginfo_t *info, void *context) {
    uintptr_t pc = (uintptr_t)((ucontext_t *)context)->uc_mcontext.pc;
    Dl_info symbol = {0};
    dladdr((void *)pc, &symbol);
    dprintf(STDERR_FILENO,
            "TILO_SIGILL pc=%p si_addr=%p si_code=%d object=%s symbol=%s base=%p\n",
            (void *)pc, info->si_addr, info->si_code,
            symbol.dli_fname ? symbol.dli_fname : "<anonymous>",
            symbol.dli_sname ? symbol.dli_sname : "<unknown>", symbol.dli_fbase);

    FILE *maps = fopen("/proc/self/maps", "r");
    if (maps) {
        char line[1024];
        while (fgets(line, sizeof(line), maps)) {
            unsigned long start, end;
            if (sscanf(line, "%lx-%lx", &start, &end) == 2 &&
                pc >= start && pc < end) {
                dprintf(STDERR_FILENO, "TILO_SIGILL mapping=%s", line);
                break;
            }
        }
        fclose(maps);
    }
    _exit(128 + signal_number);
}

__attribute__((constructor)) static void install_sigill_probe(void) {
    struct sigaction action = {0};
    action.sa_sigaction = on_sigill;
    action.sa_flags = SA_SIGINFO;
    sigemptyset(&action.sa_mask);
    sigaction(SIGILL, &action, NULL);
    dprintf(STDERR_FILENO, "TILO_SIGILL_PROBE_INSTALLED pid=%ld\n", (long)getpid());
}

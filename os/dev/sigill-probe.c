/* Disposable native-KVM diagnostic loaded directly into tilo-greeter. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

static void on_sigill(int signal_number, siginfo_t *info, void *context) {
    (void)signal_number;
    (void)context;
    Dl_info symbol = {0};
    char line[512];
    if (dladdr(info->si_addr, &symbol) && symbol.dli_fname) {
        int count = snprintf(line, sizeof(line),
            "TILO_SIGILL_PC=%p OBJECT=%s BASE=%p\n",
            info->si_addr, symbol.dli_fname, symbol.dli_fbase);
        if (count > 0) write(STDERR_FILENO, line, (size_t)count);
    } else {
        int count = snprintf(line, sizeof(line), "TILO_SIGILL_PC=%p OBJECT=unknown\n", info->si_addr);
        if (count > 0) write(STDERR_FILENO, line, (size_t)count);
    }
    int maps = open("/proc/self/maps", O_RDONLY);
    if (maps >= 0) {
        static const char marker[] = "TILO_SIGILL_MAPS_BEGIN\n";
        write(STDERR_FILENO, marker, sizeof(marker) - 1);
        char buffer[4096];
        ssize_t bytes;
        while ((bytes = read(maps, buffer, sizeof(buffer))) > 0)
            write(STDERR_FILENO, buffer, (size_t)bytes);
        close(maps);
    }
    _exit(132);
}

/* The greeter's graphics libraries replace the signal disposition after
 * loading. Keep the probe installed during this disposable diagnostic. */
int sigaction(int number, const struct sigaction *action, struct sigaction *previous) {
    static int (*next)(int, const struct sigaction *, struct sigaction *);
    if (!next) next = dlsym(RTLD_NEXT, "sigaction");
    if (number == SIGILL && action) {
        static const char marker[] = "TILO_SIGILL_HANDLER_INTERCEPTED\n";
        write(STDERR_FILENO, marker, sizeof(marker) - 1);
        struct sigaction probe = *action;
        probe.sa_sigaction = on_sigill;
        probe.sa_flags |= SA_SIGINFO;
        return next(number, &probe, previous);
    }
    return next(number, action, previous);
}

__attribute__((constructor)) static void install_sigill_probe(void) {
    static const char loaded[] = "TILO_SIGILL_PROBE_LOADED\n";
    write(STDERR_FILENO, loaded, sizeof(loaded) - 1);
    struct sigaction action = {0};
    action.sa_sigaction = on_sigill;
    sigemptyset(&action.sa_mask);
    action.sa_flags = SA_SIGINFO;
    sigaction(SIGILL, &action, NULL);
}

/* Disposable native-KVM diagnostic. Emit the executable name on SIGILL
 * without ptrace, which the guest's nested virtualization policy disallows. */
#define _GNU_SOURCE
#include <fcntl.h>
#include <signal.h>
#include <stddef.h>
#include <unistd.h>

static void on_sigill(int signal_number, siginfo_t *info, void *context) {
    (void)signal_number;
    (void)info;
    (void)context;
    char name[128] = {0};
    int fd = open("/proc/self/comm", O_RDONLY);
    ssize_t size = fd < 0 ? -1 : read(fd, name, sizeof(name));
    if (fd >= 0) close(fd);
    static const char prefix[] = "TILO_SIGILL_PROCESS=";
    write(STDERR_FILENO, prefix, sizeof(prefix) - 1);
    if (size > 0) write(STDERR_FILENO, name, (size_t)size);
    _exit(132);
}

__attribute__((constructor)) static void install_sigill_probe(void) {
    struct sigaction action = {0};
    action.sa_sigaction = on_sigill;
    sigemptyset(&action.sa_mask);
    action.sa_flags = SA_SIGINFO;
    sigaction(SIGILL, &action, NULL);
}

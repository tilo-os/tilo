// SPDX-License-Identifier: MIT
// Real fork/exec coverage of the production entrypoint supervisor.
#define main tilo_android_entry_main
#include "atl_entry.c"
#undef main

static int worker(const char *mode, const char *counter) {
    FILE *file = fopen(counter, "a");
    if (!file) fail("opening disposable worker counter");
    fprintf(file, "%s\n", getenv("TILO_ANDROID_RECOVER_SERVICES") ? "recovered" : "initial");
    fclose(file);
    int fd = atoi(getenv("TILO_ANDROID_RECOVERY_FD"));
    const char *record = !strcmp(mode, "malformed") ? "../not-a-service\n" : "org.tilo.Worker\n";
    if (strcmp(mode, "empty")) {
        size_t size = strlen(record);
        if (pwrite(fd, record, size, 0) != (ssize_t)size) fail("writing worker journal");
    }
    if (!strcmp(mode, "normal")) return 0;
    if (!strcmp(mode, "term")) raise(SIGTERM);
    if (!strcmp(mode, "recover") && getenv("TILO_ANDROID_RECOVER_SERVICES")) return 0;
    raise(SIGKILL);
    return 3;
}

int main(int argc, char **argv) {
    if (argc == 4 && !strcmp(argv[1], "--worker")) return worker(argv[2], argv[3]);
    if (argc != 3) return 2;
    char self[PATH_MAX];
    ssize_t size = readlink("/proc/self/exe", self, sizeof(self) - 1);
    if (size < 0 || size >= (ssize_t)sizeof(self) - 1) return 2;
    self[size] = '\0';
    char *child[] = {self, "--worker", argv[1], argv[2], NULL};
    return supervise(self, child);
}

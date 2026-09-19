/* Native executable for the package verifier regressions, never shipped. */
#include <dirent.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int inherited_descriptor(void) {
    DIR *descriptors = opendir("/dev/fd");
    if (!descriptors) return 1;
    int own_descriptor = dirfd(descriptors);
    struct dirent *entry;
    while ((entry = readdir(descriptors))) {
        char *end;
        long fd = strtol(entry->d_name, &end, 10);
        if (*end == '\0' && fd > 2 && fd != own_descriptor) {
            closedir(descriptors);
            return 1;
        }
    }
    closedir(descriptors);
    return 0;
}

int main(int argc, char **argv) {
    const char *mode = getenv("TILO_IMPORT_FIXTURE_MODE");
    if (!mode) mode = "valid";
    if (inherited_descriptor()) return 91;
    if (argc != 2) return 92;
    if (strcmp(argv[1], "--qos-connected-lease-receiver") == 0) {
        if (strcmp(mode, "receiver-timeout") == 0) sleep(60);
        if (strcmp(mode, "receiver-zero") == 0) return 0;
        if (strcmp(mode, "receiver-wrong") == 0) return 1;
        return 2;
    }
    if (strcmp(argv[1], "--tilo-package-smoke") == 0) {
        if (strcmp(mode, "smoke-timeout") == 0) sleep(60);
        if (strcmp(mode, "smoke-wrong") == 0) return 2;
        return 0;
    }
    return 93;
}

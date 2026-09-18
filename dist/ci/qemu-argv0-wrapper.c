/*
 * PRoot invokes QEMU with "-0 argv0 executable ...". The statically linked
 * QEMU distributed by tonistiigi/binfmt already preserves argv[0], so adapt
 * that spelling to "executable argv0 ...". Keeping this as a tiny native
 * launcher avoids runner-global binfmt_misc registration.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
    const char *qemu = getenv("TILO_QEMU_BINARY");
    if (qemu == NULL || argc < 4) {
        fputs("TILO_QEMU_BINARY or QEMU arguments missing\n", stderr);
        return 127;
    }

    char **adapted = calloc((size_t)argc + 1, sizeof(*adapted));
    if (adapted == NULL) return 127;

    int out = 0;
    const char *argv0 = NULL;
    adapted[out++] = (char *)qemu;
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "-0") == 0 && i + 1 < argc) {
            argv0 = argv[++i];
            continue;
        }
        adapted[out++] = argv[i];
        if (argv0 != NULL) {
            adapted[out++] = (char *)argv0;
            argv0 = NULL;
        }
    }

    execv(qemu, adapted);
    perror("execv QEMU");
    return 127;
}

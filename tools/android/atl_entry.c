// SPDX-License-Identifier: MIT
// Private ATL runtime entrypoint. It runs inside tilo-launcher's sandbox;
// it never creates a namespace or changes the launcher's permissions.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <pwd.h>
#include <sys/socket.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static void fail(const char *message) {
    fprintf(stderr, "tilo Android runtime: %s: %s\n", message, strerror(errno));
    exit(2);
}

static char *join(const char *root, const char *suffix) {
    char *result = NULL;
    if (asprintf(&result, "%s/%s", root, suffix) < 0) fail("allocating path");
    return result;
}

static void put(const char *key, const char *value) {
    if (setenv(key, value, 1) < 0) fail("setting runtime environment");
}

// A negative qualification fixture through the production launcher. This
// tests the launcher's boundary; it is not an Android framework test.
static int probe(const char *canary) {
    struct passwd *account = getpwuid(geteuid());
    if (!account || strcmp(account->pw_name, "app") ||
        strcmp(account->pw_dir, "/run/tilo/app")) {
        errno = EINVAL; fail("sandbox account is not synthetic");
    }
    for (int i = 0; i < 2; i++) {
        int fd = open(canary, i ? O_WRONLY : O_RDONLY);
        if (fd >= 0) { close(fd); errno = EACCES; fail("canary is accessible"); }
        if (errno != ENOENT && errno != EACCES && errno != EPERM)
            fail("unexpected canary denial");
    }
    int network = socket(AF_INET, SOCK_STREAM, 0);
    if (network < 0) fail("creating network probe");
    struct sockaddr_in address = { .sin_family = AF_INET,
        .sin_port = htons(9), .sin_addr = { .s_addr = htonl(INADDR_LOOPBACK) } };
    int result = connect(network, (struct sockaddr *)&address, sizeof(address));
    int error = errno;
    close(network);
    if (result == 0 || (error != EPERM && error != EACCES)) {
        errno = error; fail("network is not policy-denied");
    }
    puts("{\"file_read\":\"denied\",\"file_write\":\"denied\","
         "\"network\":\"denied\",\"account\":\"synthetic\"}");
    return 0;
}

int main(int argc, char **argv) {
    if (argc == 3 && !strcmp(argv[1], "--probe")) return probe(argv[2]);
    // GUI launch forwards one document argument. Naming this fixture does
    // not bind it or grant access; the launcher must still deny every open.
    const char *document = argc == 2 ? strrchr(argv[1], '/') : NULL;
    if (document && !strcmp(document + 1, "private-canary")) return probe(argv[1]);
    char root[PATH_MAX];
    ssize_t length = readlink("/proc/self/exe", root, sizeof(root) - 1);
    if (length < 0 || length >= (ssize_t)sizeof(root) - 1) fail("locating entrypoint");
    root[length] = '\0';
    // Entry lives at <private runtime>/bin/android-translation-layer.
    for (int i = 0; i < 2; i++) {
        char *slash = strrchr(root, '/');
        if (!slash) { errno = EINVAL; fail("invalid runtime layout"); }
        *slash = '\0';
    }
#if defined(__x86_64__)
    char *loader = join(root, "lib/ld-musl-x86_64.so.1");
#elif defined(__aarch64__)
    char *loader = join(root, "lib/ld-musl-aarch64.so.1");
#else
#error Unsupported Android qualification architecture
#endif
    char *binary = join(root, "usr/bin/android-translation-layer");
    char *libraries = NULL;
    if (asprintf(&libraries, "%s/usr/lib/java/dex/android_translation_layer/natives:%s/usr/lib/java/dex/art/natives:%s/usr/lib/art:%s/usr/lib:%s/lib",
                 root, root, root, root, root) < 0)
        fail("allocating library path");
    put("LD_LIBRARY_PATH", libraries);
    put("GDK_BACKEND", "wayland");
    put("ATL_DISABLE_WINDOW_DECORATIONS", "1");
    // Software rendering makes the first acceptance journey independent of
    // hardware. Hardware rendering requires a separately recorded journey.
    put("GSK_RENDERER", "cairo");
    put("XDG_DATA_DIRS", join(root, "usr/share"));
    put("GSETTINGS_SCHEMA_DIR", join(root, "usr/share/glib-2.0/schemas"));
    put("GIO_MODULE_DIR", join(root, "usr/lib/gio/modules"));
    put("FONTCONFIG_FILE", join(root, "etc/fonts/tilo-private.conf"));
    put("FONTCONFIG_PATH", join(root, "etc/fonts"));
    put("ANDROID_ROOT", join(root, "usr"));
    put("ICU_DATA", join(root, "usr/share/icu/tilo"));
    // Prefix-relative framework paths are resolved by upstream ATL from the
    // loaded libart.so; never bind the host's /usr/share or Java libraries.
    char **child = calloc((size_t)argc + 16, sizeof(char *));
    if (!child) fail("allocating argv");
    child[0] = loader;
    child[1] = "--library-path";
    child[2] = libraries;
    child[3] = binary;
    int next = 4;
    char activity[4096];
    if (argc == 1) {
        child[next++] = join(root, "../app.apk");
        FILE *file = fopen(join(root, "../activity.txt"), "r");
        if (!file) fail("opening prepared activity");
        if (!fgets(activity, sizeof(activity), file)) { errno = EINVAL; fail("reading activity"); }
        if (fgetc(file) != EOF) { errno = EINVAL; fail("oversized activity"); }
        fclose(file);
        activity[strcspn(activity, "\r\n")] = '\0';
        if (!*activity) { errno = EINVAL; fail("empty activity"); }
        for (const char *p = activity; *p; p++) {
            if (!( (*p >= 'A' && *p <= 'Z') || (*p >= 'a' && *p <= 'z') ||
                   (*p >= '0' && *p <= '9') || *p == '_' || *p == '$' || *p == '/')) {
                errno = EINVAL; fail("invalid activity");
            }
        }
        child[next++] = "-l";
        child[next++] = activity;
    }
    for (int i = 1; i < argc; i++) child[next++] = argv[i];
    const char *package = getenv("TILO_PACKAGE_ID");
    if (package && *package) {
        child[next++] = "--gapplication-app-id";
        child[next++] = (char *)package;
    }
    child[next++] = "-X";
    child[next++] = "-Xnoimage-dex2oat";
    child[next++] = "-X";
    child[next++] = "-Xusejit:false";
    execv(loader, child);
    fail("executing private musl loader");
}

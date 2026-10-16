/* Bundle-local launcher; exec preserves helper arguments and exit semantics. */
#include <errno.h>
#include <CommonCrypto/CommonDigest.h>
#include <limits.h>
#include <mach-o/dyld.h>
#include <pwd.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include "local_runtime_config.h"

#ifndef TILO_LAUNCH_NAME
#define TILO_LAUNCH_NAME "tilo-runtime-mcp"
#define TILO_EXEC_NAME "tilo-runtime-mcp.real"
#endif

static void fail(void) {
    fputs("Tilo local runtime: invalid bundle or cache path\n", stderr);
    exit(126);
}

static void join(char *out, const char *base, const char *suffix) {
    if (snprintf(out, PATH_MAX, "%s/%s", base, suffix) >= PATH_MAX) fail();
}

static void directory(const char *path) {
    struct stat st;
    if (mkdir(path, 0700) && errno != EEXIST) fail();
    if (lstat(path, &st) || !S_ISDIR(st.st_mode) || st.st_uid != getuid() ||
        (st.st_mode & 0022)) fail();
}

static void variable(const char *name, const char *value) {
    if (setenv(name, value, 1)) fail();
}

int main(int argc, char **argv) {
    (void)argc;
    char raw[PATH_MAX], executable[PATH_MAX], plugins[PATH_MAX];
    char scanner[PATH_MAX], cache[PATH_MAX], next[PATH_MAX];
    uint32_t size = sizeof(raw);
    if (_NSGetExecutablePath(raw, &size) || !realpath(raw, executable)) fail();
    char *slash = strrchr(executable, '/');
    if (!slash || strcmp(slash + 1, TILO_LAUNCH_NAME)) fail();
    *slash = 0;
    join(next, executable, TILO_EXEC_NAME);
    slash = strrchr(executable, '/');
    if (!slash || strcmp(slash + 1, "MacOS")) fail();
    *slash = 0;
    /* GStreamer uses colon-delimited path lists; do not misparse a bundle. */
    if (strchr(executable, ':')) fail();
    join(plugins, executable, "PlugIns");
    join(scanner, executable, "Helpers/gst-plugin-scanner");
    struct passwd *user = getpwuid(getuid());
    if (!user || !user->pw_dir || user->pw_dir[0] != '/') fail();
    join(cache, user->pw_dir, "Library"); directory(cache);
    join(raw, cache, "Caches"); directory(raw);
    join(cache, raw, "so.tilo.local-runtime"); directory(cache);
    join(raw, cache, TILO_PROFILE_KEY); directory(raw);
    /* Registry entries carry absolute plugin paths. Distinct app bundles and
     * moved copies must not reuse another location's registry. */
    unsigned char digest[CC_SHA256_DIGEST_LENGTH];
    char location[CC_SHA256_DIGEST_LENGTH * 2 + 1];
    if (!CC_SHA256(plugins, (CC_LONG)strlen(plugins), digest)) fail();
    for (size_t i = 0; i < sizeof(digest); i++)
        snprintf(location + 2 * i, 3, "%02x", digest[i]);
    join(cache, raw, location); directory(cache);
#ifdef TILO_MEDIA_DATA
    /* Restrict subtitle configuration/data to this signed bundle. System font
     * files are read in place; caches stay outside the sealed bundle. */
    variable("XDG_CACHE_HOME", cache);
    join(raw, executable, "Resources/fontconfig");
    variable("FONTCONFIG_PATH", raw);
    join(raw, executable, "Resources/fontconfig/fonts.conf");
    if (access(raw, R_OK)) fail();
    variable("FONTCONFIG_FILE", raw);
    unsetenv("FONTCONFIG_SYSROOT");
    variable("PANGOCAIRO_BACKEND", "fontconfig");
    join(raw, executable, "Resources/libthai/thbrk.tri");
    if (access(raw, R_OK)) fail();
    join(raw, executable, "Resources/libthai");
    variable("LIBTHAI_DICTDIR", raw);
    /* reqwest's native certificate loader uses macOS system trust. */
    unsetenv("SSL_CERT_FILE");
    unsetenv("SSL_CERT_DIR");
#endif
    join(raw, cache, "registry-arm64.bin");
    variable("GST_PLUGIN_SYSTEM_PATH_1_0", plugins);
    variable("GST_PLUGIN_SYSTEM_PATH", plugins);
    variable("GST_PLUGIN_PATH_1_0", "");
    variable("GST_PLUGIN_PATH", "");
    variable("GST_PLUGIN_SCANNER_1_0", scanner);
    variable("GST_PLUGIN_SCANNER", scanner);
    variable("GST_REGISTRY_1_0", raw);
    variable("GST_REGISTRY", raw);
    /* A stale external cache must not suppress local scanner validation. */
    variable("GST_REGISTRY_UPDATE", "yes");
    variable("GST_REGISTRY_FORK", "yes");
    unsetenv("GST_PLUGIN_FEATURE_RANK");
    execv(next, argv);
    perror("Tilo local runtime exec");
    return 126;
}

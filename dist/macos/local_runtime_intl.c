/* Private local runtime adapter. All other libintl exports are re-exported.
 * No DYLD injection or entitlement exception is required. */
#include <dlfcn.h>
#include <errno.h>
#include <limits.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "local_runtime_config.h"

typedef char *(*bind_fn)(const char *, const char *);
static pthread_once_t once = PTHREAD_ONCE_INIT;
static bind_fn original;
static char locales[PATH_MAX];

char *libintl_bindtextdomain(const char *domain, const char *directory);

static void initialize(void) {
    Dl_info info;
    char library[PATH_MAX], real_library[PATH_MAX];
    if (!dladdr((void *)&libintl_bindtextdomain, &info) ||
        !realpath(info.dli_fname, library)) return;
    char *slash = strrchr(library, '/');
    if (!slash) return;
    *slash = 0;
    if (snprintf(real_library, sizeof(real_library), "%s/%s", library,
                 TILO_INTL_NAME) >= (int)sizeof(real_library) ||
        snprintf(locales, sizeof(locales), "%s/../Resources/locale", library) >=
            (int)sizeof(locales)) return;
    char canonical[PATH_MAX];
    if (!realpath(locales, canonical)) return;
    memcpy(locales, canonical, strlen(canonical) + 1);
    void *handle = dlopen(real_library, RTLD_NOW | RTLD_LOCAL);
    if (!handle) return;
    bind_fn resolved = (bind_fn)dlsym(handle, "libintl_bindtextdomain");
    if (resolved != &libintl_bindtextdomain) original = resolved;
    /* Keep the library handle alive for the process lifetime. */
}

char *libintl_bindtextdomain(const char *domain, const char *directory) {
    pthread_once(&once, initialize);
    if (!original) { errno = EINVAL; return NULL; }
    if (domain && directory) {
        int gst = !strcmp(domain, "gstreamer-1.0") ||
                  !strcmp(domain, "gst-plugins-base-1.0");
#ifdef TILO_GST_EXTENDED_CATALOGS
        gst = gst || !strcmp(domain, "gst-plugins-good-1.0") ||
                     !strcmp(domain, "gst-plugins-bad-1.0") ||
                     !strcmp(domain, "gst-plugins-ugly-1.0");
#endif
        int glib = !strcmp(domain, "glib20");
        if ((gst && !strcmp(directory, TILO_GST_LOCALE)) ||
            (glib && !strcmp(directory, TILO_GLIB_LOCALE))) directory = locales;
    }
    return original(domain, directory);
}

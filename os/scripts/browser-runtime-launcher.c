/* The browser executable lives in its signed runtime bundle.  Keep the app
 * package small and enter the same executable through the ordinary launcher
 * sandbox; the bundle is mounted read-only at this fixed path. */
#include <stdio.h>
#include <unistd.h>

int main(int argc, char **argv) {
    (void)argc;
    const char *browser = "/run/tilo-runtime/2026.7.0-browser/lib/browser";
    execv(browser, argv);
    perror("browser runtime exec");
    return 127;
}

/* Mesa's shared LLVM JIT must use baseline ARM instructions on virtual 2D
 * GPUs. A Neoverse V2 guest may expose its CPU name while masking SVE;
 * LLVM's CPU model otherwise emits SVE (addvl) and the greeter receives SIGILL.
 * Loaded only by tilo-cage-session for ARM virtio graphics, not physical GPUs.
 */
#include <stddef.h>
#ifdef TILO_GALLIVM_DIAGNOSTIC
#include <unistd.h>
#endif

struct llvm_string_ref {
    const char *data;
    size_t length;
};

__attribute__((visibility("default")))
struct llvm_string_ref _ZN4llvm3sys14getHostCPUNameEv(void) {
    static const char name[] = "generic";
#ifdef TILO_GALLIVM_DIAGNOSTIC
    static const char message[] = "TILO_GALLIVM_CPU=generic\n";
    static int reported;
    if (!reported) {
        reported = 1;
        (void)write(STDERR_FILENO, message, sizeof(message) - 1);
    }
#endif
    return (struct llvm_string_ref){name, sizeof(name) - 1};
}

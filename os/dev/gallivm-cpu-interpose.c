/* Diagnostic only: make Mesa's shared LLVM JIT choose a baseline ARM CPU. */
#include <stddef.h>
#include <unistd.h>

struct llvm_string_ref {
    const char *data;
    size_t length;
};

__attribute__((visibility("default")))
struct llvm_string_ref _ZN4llvm3sys14getHostCPUNameEv(void) {
    static const char name[] = "generic";
    static const char message[] = "TILO_GALLIVM_CPU=generic\n";
    static int reported;
    if (!reported) {
        reported = 1;
        (void)write(STDERR_FILENO, message, sizeof(message) - 1);
    }
    return (struct llvm_string_ref){name, sizeof(name) - 1};
}

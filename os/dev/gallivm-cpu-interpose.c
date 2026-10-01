/* Diagnostic only: make Mesa's shared LLVM JIT choose a baseline ARM CPU. */
#include <stddef.h>
#include <unistd.h>

struct llvm_string_ref {
    const char *data;
    size_t length;
};

/* LLVM 20's StringMap<bool> is 24 bytes on aarch64. An empty map has no
 * buckets, so Mesa's feature loop cannot re-enable SVE for a generic CPU. */
struct llvm_string_map_bool {
    void *table;
    unsigned buckets;
    unsigned items;
    unsigned tombstones;
    unsigned item_size;
};
_Static_assert(sizeof(struct llvm_string_map_bool) == 24,
               "LLVM 20 aarch64 StringMap layout changed");

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

__attribute__((visibility("default")))
struct llvm_string_map_bool _ZN4llvm3sys18getHostCPUFeaturesEv(void) {
    static const char message[] = "TILO_GALLIVM_FEATURES=empty\n";
    static int reported;
    if (!reported) {
        reported = 1;
        (void)write(STDERR_FILENO, message, sizeof(message) - 1);
    }
    return (struct llvm_string_map_bool){NULL, 0, 0, 0, 16};
}

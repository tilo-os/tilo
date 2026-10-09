// Review-only configuration shared by the authenticated factory and worker.
// Signal-based traps and heap/table Spectre mitigations retain their defaults.
#pragma once
#include <wasmtime.h>
#include <stdexcept>

inline void aot_check(wasmtime_error_t* error) {
  if (error) {
    wasmtime_error_delete(error);
    throw std::runtime_error("Wasmtime operation failed");
  }
}
inline wasm_engine_t* aot_engine() {
  auto* config = wasm_config_new();
  if (!config) throw std::runtime_error("configuration allocation");
  aot_check(wasmtime_config_target_set(config, "x86_64-unknown-linux-gnu"));
  // Explicit OS CPU floor; setting a target disables builder CPU inference.
  for (const char* flag : {"has_sse3", "has_ssse3", "has_sse41", "has_sse42",
      "has_popcnt", "has_cmpxchg16b", "has_avx", "has_avx2", "has_fma",
      "has_bmi1", "has_bmi2", "has_lzcnt"})
    wasmtime_config_cranelift_flag_set(config, flag, "true");
  wasmtime_config_cranelift_opt_level_set(config, WASMTIME_OPT_LEVEL_SPEED);
  wasmtime_config_memory_reservation_set(config, 0);
  wasmtime_config_memory_guard_size_set(config, 0);
  wasmtime_config_memory_reservation_for_growth_set(config, 0);
  auto* engine = wasm_engine_new_with_config(config);
  if (!engine) throw std::runtime_error("engine allocation");
  return engine;
}

// Source-built review factory. No model, audio, or publisher host code executes.
// The outer controller authenticates the exact pinned Wasm before invoking this.
#include <initializer_list>
#include "whistle_aot_config.hpp"
#include <chrono>
#include <fstream>
#include <iostream>
#include <memory>
#include <vector>

int main(int argc, char** argv) {
  try {
    if (argc != 3) return 2;
    std::ifstream input(argv[1], std::ios::binary);
    std::vector<uint8_t> bytes(923348);
    if (!input.read(reinterpret_cast<char*>(bytes.data()), bytes.size()) ||
        input.peek() != std::char_traits<char>::eof()) return 2;
    auto engine = std::unique_ptr<wasm_engine_t, decltype(&wasm_engine_delete)>(
        aot_engine(), wasm_engine_delete);
    const auto start = std::chrono::steady_clock::now();
    wasmtime_module_t* raw = nullptr;
    aot_check(wasmtime_module_new(engine.get(), bytes.data(), bytes.size(), &raw));
    auto module = std::unique_ptr<wasmtime_module_t, decltype(&wasmtime_module_delete)>(
        raw, wasmtime_module_delete);
    wasm_byte_vec_t artifact;
    aot_check(wasmtime_module_serialize(module.get(), &artifact));
    std::ofstream output(argv[2], std::ios::binary | std::ios::trunc);
    output.write(artifact.data, artifact.size);
    const auto size = artifact.size;
    wasm_byte_vec_delete(&artifact);
    if (!output) return 2;
    std::cout << "WHISTLE_AOT_COMPILE {\"seconds\":"
        << std::chrono::duration<double>(std::chrono::steady_clock::now() - start).count()
        << ",\"bytes\":" << size << ",\"target\":\"x86-64-v3\","
        << "\"signal_and_spectre_defaults\":true}" << std::endl;
    return 0;
  } catch (...) {
    std::cerr << "authenticated AOT factory failed" << std::endl;
    return 1;
  }
}

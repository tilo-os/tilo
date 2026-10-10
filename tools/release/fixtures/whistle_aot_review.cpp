// Default-off public qualification ABI, not a production runtime admission.
// Deserialize only the outer controller's freshly compiled, authenticated AOT.
#include <initializer_list>
#include "whistle_aot_config.hpp"
#include <array>
#include <chrono>
#include <cstdint>
#include <cstring>
#include <memory>
#include <string>
#include <string_view>
#include <vector>

namespace {
constexpr size_t memory_limit = 512 * 1024 * 1024;
void erase(void* pointer, size_t size) {
  auto* bytes = static_cast<volatile uint8_t*>(pointer);
  while (size--) *bytes++ = 0;
}
struct SensitiveString {
  std::string value;
  ~SensitiveString() { erase(value.data(), value.size()); }
};
wasmtime_val_t integer(int32_t value) {
  wasmtime_val_t result{};
  result.kind = WASMTIME_I32;
  result.of.i32 = value;
  return result;
}
struct Runtime {
  wasm_engine_t* engine = nullptr;
  wasmtime_module_t* module = nullptr;
  wasmtime_store_t* store = nullptr;
  wasmtime_context_t* context = nullptr;
  wasmtime_instance_t instance{};
  wasmtime_memory_t memory{};
  bool has_memory = false;
  std::array<char, 16> hosts{};
  uint32_t pcm = 0, output = 0, language = 0, keywords = 0;
  ~Runtime() {
    if (has_memory && context) {
      const size_t size = wasmtime_memory_data_size(context, &memory);
      if (size <= memory_limit) erase(wasmtime_memory_data(context, &memory), size);
    }
    if (store) wasmtime_store_delete(store);
    if (module) wasmtime_module_delete(module);
    if (engine) wasm_engine_delete(engine);
  }
  uint8_t* range(uint32_t offset, size_t size) {
    const size_t length = wasmtime_memory_data_size(context, &memory);
    if (offset > length || size > length - offset || length > memory_limit)
      throw std::runtime_error("guest memory bounds");
    return wasmtime_memory_data(context, &memory) + offset;
  }
  void write32(uint32_t offset, uint32_t value) {
    std::memcpy(range(offset, 4), &value, 4);
  }
  int32_t invoke(const char* name, std::initializer_list<wasmtime_val_t> args) {
    wasmtime_extern_t item;
    if (!wasmtime_instance_export_get(context, &instance, name, std::strlen(name), &item)
        || item.kind != WASMTIME_EXTERN_FUNC) throw std::runtime_error("guest export");
    wasmtime_val_t result{};
    wasm_trap_t* trap = nullptr;
    const bool constructor = std::strcmp(name, "r") == 0;
    auto* error = wasmtime_func_call(context, &item.of.func, args.begin(), args.size(),
        &result, constructor ? 0 : 1, &trap);
    if (trap) {
      wasm_trap_delete(trap);
      if (error) wasmtime_error_delete(error);
      throw std::runtime_error("guest trap");
    }
    aot_check(error);
    if (constructor) return 0;
    if (result.kind != WASMTIME_I32) throw std::runtime_error("guest result");
    return result.of.i32;
  }
  uint32_t allocate(size_t size) {
    if (!size || size > memory_limit) throw std::runtime_error("allocation size");
    const int32_t pointer = invoke("E", {integer(static_cast<int32_t>(size))});
    if (pointer <= 0) throw std::runtime_error("guest allocation");
    range(pointer, size);
    return pointer;
  }
};
// Wasmtime owns callback invocation; exceptions never cross its Rust frames.
wasm_trap_t* host(void* environment, wasmtime_caller_t* caller,
    const wasmtime_val_t* args, size_t count, wasmtime_val_t* results, size_t returns) {
  try {
    const char name = *static_cast<char*>(environment);
    auto* context = wasmtime_caller_context(caller);
    wasmtime_extern_t item;
    if (!wasmtime_caller_export_get(caller, "q", 1, &item)
        || item.kind != WASMTIME_EXTERN_MEMORY) throw std::runtime_error("host memory");
    auto memory = item.of.memory;
    const auto range = [&](uint32_t offset, size_t size) -> uint8_t* {
      const size_t length = wasmtime_memory_data_size(context, &memory);
      if (length > memory_limit || offset > length || size > length - offset)
        throw std::runtime_error("host bounds");
      return wasmtime_memory_data(context, &memory) + offset;
    };
    const auto arg = [&](size_t index) -> uint32_t {
      if (index >= count || args[index].kind != WASMTIME_I32)
        throw std::runtime_error("host signature");
      return static_cast<uint32_t>(args[index].of.i32);
    };
    const auto write32 = [&](uint32_t offset, uint32_t value) {
      std::memcpy(range(offset, 4), &value, 4);
    };
    int32_t result = 0;
    switch (name) {
      case 'a': case 'e': throw std::runtime_error("guest abort");
      case 'd': case 'i': case 'p': case 'n': case 'o': result = -52; break;
      case 'f': result = -8; break;
      case 'j': break;
      case 'k': write32(arg(0), 0); write32(arg(1), 0); break;
      case 'l': {
        if (arg(0) > 1) throw std::runtime_error("clock kind");
        const uint64_t value = arg(0) == 0
            ? std::chrono::duration_cast<std::chrono::nanoseconds>(
                std::chrono::system_clock::now().time_since_epoch()).count()
            : std::chrono::duration_cast<std::chrono::nanoseconds>(
                std::chrono::steady_clock::now().time_since_epoch()).count();
        std::memcpy(range(arg(2), 8), &value, 8);
        break;
      }
      case 'm': {
        const size_t wanted = arg(0);
        const size_t current = wasmtime_memory_data_size(context, &memory);
        result = 0;
        if (wanted <= memory_limit) {
          uint64_t previous;
          auto* error = wanted > current ? wasmtime_memory_grow(context, &memory,
              (wanted + 65535) / 65536 - current / 65536, &previous) : nullptr;
          result = error ? 0 : 1;
          if (error) wasmtime_error_delete(error);
        }
        break;
      }
      case 'b': result = arg(0) <= 2 ? 0 : 8; break;
      case 'g': result = 70; break;
      case 'h':
        if (arg(0)) throw std::runtime_error("file read");
        write32(arg(3), 0); break;
      case 'c': {
        if ((arg(0) != 1 && arg(0) != 2) || arg(2) > 16)
          throw std::runtime_error("diagnostic vectors");
        const auto* vectors = range(arg(1), arg(2) * 8);
        uint32_t total = 0;
        for (uint32_t index = 0; index < arg(2); ++index) {
          uint32_t offset, size;
          std::memcpy(&offset, vectors + index * 8, 4);
          std::memcpy(&size, vectors + index * 8 + 4, 4);
          range(offset, size);
          if (size > 65536 - total) throw std::runtime_error("diagnostic size");
          total += size;
        }
        write32(arg(3), total); break;
      }
      default: throw std::runtime_error("unknown host");
    }
    if (returns != 1) throw std::runtime_error("host result count");
    results[0] = integer(result);
    return nullptr;
  } catch (...) { return wasmtime_trap_new("bounded host failure", 20); }
}
void initialize(Runtime& runtime, const uint8_t* artifact, size_t size) {
  runtime.engine = aot_engine();
  aot_check(wasmtime_module_deserialize(runtime.engine, artifact, size, &runtime.module));
  runtime.store = wasmtime_store_new(runtime.engine, nullptr, nullptr);
  if (!runtime.store) throw std::runtime_error("store allocation");
  wasmtime_store_limiter(runtime.store, memory_limit, 65536, 1, 1, 1);
  runtime.context = wasmtime_store_context(runtime.store);
  wasm_importtype_vec_t types;
  wasmtime_module_imports(runtime.module, &types);
  auto cleanup = std::unique_ptr<wasm_importtype_vec_t, decltype(&wasm_importtype_vec_delete)>(
      &types, wasm_importtype_vec_delete);
  if (types.size != 16) throw std::runtime_error("import count");
  std::vector<wasmtime_extern_t> imports(types.size);
  std::array<bool, 16> seen{};
  for (size_t index = 0; index < types.size; ++index) {
    const auto* space = wasm_importtype_module(types.data[index]);
    const auto* name = wasm_importtype_name(types.data[index]);
    const auto* type = wasm_externtype_as_functype_const(wasm_importtype_type(types.data[index]));
    if (space->size != 1 || space->data[0] != 'a' || name->size != 1
        || name->data[0] < 'a' || name->data[0] > 'p' || !type)
      throw std::runtime_error("import policy");
    const size_t slot = name->data[0] - 'a';
    if (seen[slot]) throw std::runtime_error("duplicate import");
    seen[slot] = true;
    runtime.hosts[index] = name->data[0];
    imports[index].kind = WASMTIME_EXTERN_FUNC;
    wasmtime_func_new(runtime.context, type, host, &runtime.hosts[index], nullptr,
        &imports[index].of.func);
  }
  wasm_trap_t* trap = nullptr;
  auto* error = wasmtime_instance_new(runtime.context, runtime.module, imports.data(),
      imports.size(), &runtime.instance, &trap);
  if (trap) {
    wasm_trap_delete(trap);
    if (error) wasmtime_error_delete(error);
    throw std::runtime_error("instance trap");
  }
  aot_check(error);
  wasmtime_extern_t memory;
  if (!wasmtime_instance_export_get(runtime.context, &runtime.instance, "q", 1, &memory)
      || memory.kind != WASMTIME_EXTERN_MEMORY) throw std::runtime_error("memory export");
  runtime.memory = memory.of.memory;
  runtime.has_memory = true;
  runtime.invoke("r", {});
}
}
extern "C" void* tilo_whistle_review_new(const uint8_t* artifact, size_t artifact_size,
    const uint8_t* model, size_t model_size, const char* configuration, size_t configuration_size) noexcept {
  try {
    if (!artifact || !artifact_size || artifact_size > 32 * 1024 * 1024 || !model
        || model_size != 16919407 || !configuration || configuration_size > 32768) return nullptr;
    SensitiveString configuration_owner;
    auto& config = configuration_owner.value;
    config.assign(configuration, configuration_size);
    const size_t separator = config.find('\0');
    SensitiveString language_owner, keywords_owner;
    auto& language = language_owner.value;
    auto& keywords = keywords_owner.value;
    language.assign(config.data(), separator == std::string::npos ? config.size() : separator);
    constexpr std::array<std::string_view, 8> languages{"auto", "en", "de", "fr", "es", "it", "nl", "pl"};
    bool supported = false;
    for (const auto candidate : languages) if (language == candidate) supported = true;
    if (!supported) return nullptr;
    if (separator != std::string::npos) {
      if (config.back() != '\0' || separator == config.size() - 1) return nullptr;
      keywords.assign(config.data() + separator + 1, config.size() - separator - 2);
      if (keywords.find('\0') != std::string::npos || keywords.size() > 16384) return nullptr;
    }
    auto runtime = std::make_unique<Runtime>();
    initialize(*runtime, artifact, artifact_size);
    const auto pointer = runtime->allocate(model_size);
    std::memcpy(runtime->range(pointer, model_size), model, model_size);
    wasmtime_val_t length{};
    length.kind = WASMTIME_I64;
    length.of.i64 = model_size;
    if (runtime->invoke("y", {integer(pointer), length}) < 0
        || !(runtime->invoke("z", {}) & 2)) return nullptr;
    runtime->pcm = runtime->allocate(30 * 16000 * sizeof(float));
    runtime->output = runtime->allocate(65536);
    if (language != "auto") {
      runtime->language = runtime->allocate(language.size() + 1);
      std::memcpy(runtime->range(runtime->language, language.size() + 1),
          language.c_str(), language.size() + 1);
    }
    if (!keywords.empty()) {
      runtime->keywords = runtime->allocate(keywords.size() + 1);
      std::memcpy(runtime->range(runtime->keywords, keywords.size() + 1),
          keywords.c_str(), keywords.size() + 1);
    }
    return runtime.release();
  } catch (...) { return nullptr; }
}
extern "C" int tilo_whistle_review_call(void* pointer, int operation,
    const float* samples, size_t count, char* output, size_t capacity) noexcept {
  try {
    if (!pointer || !samples || !count || count > 30 * 16000 || !output
        || !capacity || capacity > 65536 || operation < 0 || operation > 2) return -1;
    auto& runtime = *static_cast<Runtime*>(pointer);
    std::memcpy(runtime.range(runtime.pcm, count * sizeof(float)), samples, count * sizeof(float));
    std::memset(runtime.range(runtime.output, 65536), 0, 65536);
    if (operation == 1 && runtime.invoke("w", {integer(runtime.output), integer(65536)}) < 0)
      return -1;
    std::memset(runtime.range(runtime.output, 65536), 0, 65536);
    const int32_t status = operation == 0
        ? runtime.invoke("v", {integer(runtime.pcm), integer(count), integer(runtime.language),
            integer(runtime.keywords), integer(runtime.output), integer(65536)})
        : runtime.invoke("x", {integer(runtime.pcm), integer(count), integer(runtime.language),
            integer(runtime.keywords), integer(1), integer(runtime.output), integer(65536)});
    if (status < 0) return -1;
    const auto* bytes = reinterpret_cast<char*>(runtime.range(runtime.output, 65536));
    const size_t length = strnlen(bytes, 65536);
    if (length >= capacity) return -1;
    std::memcpy(output, bytes, length);
    erase(runtime.range(runtime.output, 65536), 65536);
    erase(runtime.range(runtime.pcm, count * sizeof(float)), count * sizeof(float));
    return length;
  } catch (...) { return -1; }
}
extern "C" void tilo_whistle_review_free(void* pointer) noexcept {
  delete static_cast<Runtime*>(pointer);
}

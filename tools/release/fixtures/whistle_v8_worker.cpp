// Opt-in qualification ABI only. The library is never installed or admitted.
#include <libplatform/libplatform.h>
#include <v8.h>
#include <algorithm>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <memory>
#include <stdexcept>
#include <string>

namespace {
#include "whistle_v8_worker.inc"
using Clock = std::chrono::steady_clock;
const auto origin = Clock::now();

v8::Local<v8::String> string(v8::Isolate* isolate, const char* data, std::size_t size) {
  v8::Local<v8::String> result;
  if (size > 65536 || !v8::String::NewFromUtf8(isolate, data,
          v8::NewStringType::kNormal, static_cast<int>(size)).ToLocal(&result))
    throw std::runtime_error("bounded string");
  return result;
}
v8::Local<v8::String> name(v8::Isolate* isolate, const char* data) {
  return string(isolate, data, std::strlen(data));
}
void clock_seconds(const v8::FunctionCallbackInfo<v8::Value>& args) {
  args.GetReturnValue().Set(std::chrono::duration<double>(Clock::now() - origin).count());
}
v8::Local<v8::ArrayBuffer> buffer(v8::Isolate* isolate, const void* data, std::size_t size) {
  auto backing = v8::ArrayBuffer::NewBackingStore(isolate, size);
  if (size) std::memcpy(backing->Data(), data, size);
  return v8::ArrayBuffer::New(isolate, std::move(backing));
}
struct Decoder {
  std::unique_ptr<v8::Platform> platform;
  std::unique_ptr<v8::ArrayBuffer::Allocator> allocator;
  v8::Isolate* isolate = nullptr;
  v8::Global<v8::Context> context;
  ~Decoder() {
    context.Reset();
    if (isolate) isolate->Dispose();
    v8::V8::Dispose();
    v8::V8::DisposePlatform();
  }
};
}

extern "C" void* tilo_whistle_review_new(const std::uint8_t* module, std::size_t module_size,
    const std::uint8_t* model, std::size_t model_size, const char* configuration,
    std::size_t configuration_size) noexcept {
  try {
    if (!module || !model || !configuration || module_size != 923348 ||
        model_size != 16919407 || configuration_size > 32768) return nullptr;
    auto decoder = std::make_unique<Decoder>();
    v8::V8::SetFlagsFromString(
        "--single-threaded --single-threaded-gc --no-wasm-async-compilation "
        "--no-wasm-lazy-compilation --no-liftoff --no-wasm-tier-up "
        "--wasm-max-mem-pages=8192 --max-old-space-size=64");
    decoder->platform = v8::platform::NewSingleThreadedDefaultPlatform();
    v8::V8::InitializePlatform(decoder->platform.get());
    v8::V8::Initialize();
    decoder->allocator.reset(v8::ArrayBuffer::Allocator::NewDefaultAllocator());
    v8::Isolate::CreateParams parameters;
    parameters.array_buffer_allocator = decoder->allocator.get();
    parameters.constraints.ConfigureDefaultsFromHeapSize(0, 64 * 1024 * 1024);
    parameters.constraints.set_code_range_size_in_bytes(32 * 1024 * 1024);
    auto* isolate = decoder->isolate = v8::Isolate::New(parameters);
    v8::Isolate::Scope scope(isolate);
    v8::HandleScope handles(isolate);
    auto global = v8::ObjectTemplate::New(isolate);
    global->Set(isolate, "workerClock", v8::FunctionTemplate::New(isolate, clock_seconds));
    auto context = v8::Context::New(isolate, nullptr, global);
    v8::Context::Scope context_scope(context);
    v8::TryCatch errors(isolate);
    if (!context->Global()->Set(context, name(isolate, "moduleBytes"),
            buffer(isolate, module, module_size)).FromMaybe(false) ||
        !context->Global()->Set(context, name(isolate, "modelBytes"),
            buffer(isolate, model, model_size)).FromMaybe(false)) return nullptr;
    if (!context->Global()->Set(context, name(isolate, "configurationJSON"),
            string(isolate, configuration, configuration_size)).FromMaybe(false)) return nullptr;
    v8::Local<v8::Script> program;
    if (!v8::Script::Compile(context, string(isolate, worker_source,
            sizeof(worker_source) - 1)).ToLocal(&program) || program->Run(context).IsEmpty()) return nullptr;
    decoder->context.Reset(isolate, context);
    return decoder.release();
  } catch (...) { return nullptr; }
}

extern "C" int tilo_whistle_review_call(void* pointer, int operation,
    const float* samples, std::size_t count, char* output, std::size_t capacity) noexcept {
  try {
    if (!pointer || !output || !capacity || capacity > 16385 || count > 30 * 16000 ||
        (count && !samples) || (operation != 0 && operation != 1)) return -1;
    auto& decoder = *static_cast<Decoder*>(pointer);
    auto* isolate = decoder.isolate;
    v8::Isolate::Scope scope(isolate);
    v8::HandleScope handles(isolate);
    auto context = decoder.context.Get(isolate);
    v8::Context::Scope context_scope(context);
    v8::TryCatch errors(isolate);
    v8::Local<v8::Value> function;
    if (!context->Global()->Get(context, name(isolate, "workerCall")).ToLocal(&function) ||
        !function->IsFunction()) return -1;
    v8::Local<v8::Value> arguments[] = {v8::Integer::New(isolate, operation),
        buffer(isolate, samples, count * sizeof(float))};
    v8::Local<v8::Value> result;
    if (!function.As<v8::Function>()->Call(context, context->Global(), 2, arguments).ToLocal(&result) ||
        !result->IsString()) return -1;
    v8::String::Utf8Value value(isolate, result);
    if (!*value || value.length() < 0 || static_cast<std::size_t>(value.length()) >= capacity) return -1;
    std::memcpy(output, *value, value.length());
    return value.length();
  } catch (...) { return -1; }
}
extern "C" void tilo_whistle_review_free(void* pointer) noexcept {
  delete static_cast<Decoder*>(pointer);
}

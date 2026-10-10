// Private public-fixture qualification only; no Node launcher or production admission.
// The matching Node/V8 public headers and existing library are external review inputs.
#include <libplatform/libplatform.h>
#include <v8.h>

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cmath>
#include <fstream>
#include <iostream>
#include <memory>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>

namespace {
using Clock = std::chrono::steady_clock;
const auto origin = Clock::now();

std::vector<char> read_bounded(const char* path, std::size_t limit) {
  std::ifstream stream(path, std::ios::binary | std::ios::ate);
  if (!stream) throw std::runtime_error("missing public review input");
  const auto size = stream.tellg();
  if (size <= 0 || static_cast<std::uint64_t>(size) > limit)
    throw std::runtime_error("public review input size bound");
  std::vector<char> bytes(static_cast<std::size_t>(size));
  stream.seekg(0);
  if (!stream.read(bytes.data(), size) || stream.peek() != EOF)
    throw std::runtime_error("public review input changed during read");
  return bytes;
}

v8::Local<v8::String> text(v8::Isolate* isolate, const char* value) {
  return v8::String::NewFromUtf8(isolate, value).ToLocalChecked();
}

void print(const v8::FunctionCallbackInfo<v8::Value>& args) {
  if (args.Length() != 1) return;
  v8::String::Utf8Value value(args.GetIsolate(), args[0]);
  if (!*value || value.length() > 65536) {
    args.GetIsolate()->ThrowException(text(args.GetIsolate(), "bounded output"));
    return;
  }
  std::cout.write(*value, value.length());
  std::cout << '\n' << std::flush;
}

void clock_seconds(const v8::FunctionCallbackInfo<v8::Value>& args) {
  args.GetReturnValue().Set(std::chrono::duration<double>(Clock::now() - origin).count());
}

void pause(const v8::FunctionCallbackInfo<v8::Value>& args) {
  const double seconds = args.Length() == 1
      ? args[0]->NumberValue(args.GetIsolate()->GetCurrentContext()).FromMaybe(-1)
      : -1;
  if (!std::isfinite(seconds) || seconds < 0 || seconds > 5) {
    args.GetIsolate()->ThrowException(text(args.GetIsolate(), "bounded pacing"));
    return;
  }
  std::this_thread::sleep_for(std::chrono::duration<double>(seconds));
}

void buffer(v8::Local<v8::Context> context, const char* name,
            const std::vector<char>& bytes) {
  auto* isolate = context->GetIsolate();
  auto backing = v8::ArrayBuffer::NewBackingStore(isolate, bytes.size());
  std::copy(bytes.begin(), bytes.end(), static_cast<char*>(backing->Data()));
  auto value = v8::ArrayBuffer::New(isolate, std::move(backing));
  if (!context->Global()->Set(context, text(isolate, name), value).FromMaybe(false))
    throw std::runtime_error("review buffer binding failed");
}
}  // namespace

int main(int argc, char** argv) {
  try {
    if (argc < 6 || argc > 8)
      throw std::runtime_error("script module model public-f32 language [public-name-cue] [--endpoint|--lifecycle] required");
    std::string prompt;
    std::string mode = "stream";
    for (int index = 6; index < argc; ++index) {
      const std::string option = argv[index];
      if (option == "--endpoint" || option == "--lifecycle") {
        if (mode != "stream") throw std::runtime_error("duplicate review mode");
        mode = option.substr(2);
      } else if (prompt.empty()) {
        prompt = option;
      } else {
        throw std::runtime_error("duplicate public name cue");
      }
    }
    const auto script = read_bounded(argv[1], 65536);
    const auto module = read_bounded(argv[2], 923348);
    const auto model = read_bounded(argv[3], 16919407);
    const auto audio = read_bounded(argv[4], 30 * 16000 * 4);
    // Existing V8 build configuration is unchanged. No signal/trap-handler
    // activation: WebAssembly retains inline bounds checks, avoiding its large
    // virtual reservation. No Node initialization, workers or host filesystem API.
    v8::V8::SetFlagsFromString(
        "--single-threaded --single-threaded-gc --no-wasm-async-compilation "
        "--no-wasm-lazy-compilation --no-liftoff --no-wasm-tier-up "
        "--wasm-max-mem-pages=8192 --max-old-space-size=64");
    auto platform = v8::platform::NewSingleThreadedDefaultPlatform();
    v8::V8::InitializePlatform(platform.get());
    v8::V8::Initialize();
    auto allocator = std::unique_ptr<v8::ArrayBuffer::Allocator>(
        v8::ArrayBuffer::Allocator::NewDefaultAllocator());
    v8::Isolate::CreateParams parameters;
    parameters.array_buffer_allocator = allocator.get();
    parameters.constraints.ConfigureDefaultsFromHeapSize(0, 64 * 1024 * 1024);
    parameters.constraints.set_code_range_size_in_bytes(32 * 1024 * 1024);
    auto* isolate = v8::Isolate::New(parameters);
    int status = 0;
    {
      v8::Isolate::Scope isolate_scope(isolate);
      v8::HandleScope handles(isolate);
      auto global = v8::ObjectTemplate::New(isolate);
      global->Set(isolate, "reviewPrint", v8::FunctionTemplate::New(isolate, print));
      global->Set(isolate, "reviewClock", v8::FunctionTemplate::New(isolate, clock_seconds));
      global->Set(isolate, "reviewPause", v8::FunctionTemplate::New(isolate, pause));
      auto context = v8::Context::New(isolate, nullptr, global);
      v8::Context::Scope context_scope(context);
      buffer(context, "moduleBytes", module);
      buffer(context, "modelBytes", model);
      buffer(context, "audioBytes", audio);
      if (!context->Global()->Set(context, text(isolate, "reviewLanguage"),
                                 text(isolate, argv[5])).FromMaybe(false) ||
          !context->Global()->Set(context, text(isolate, "reviewPrompt"),
                                 text(isolate, prompt.c_str())).FromMaybe(false) ||
          !context->Global()->Set(context, text(isolate, "reviewMode"),
                                 text(isolate, mode.c_str())).FromMaybe(false))
        throw std::runtime_error("review language binding failed");
      v8::TryCatch errors(isolate);
      v8::Local<v8::Script> program;
      auto source = v8::String::NewFromUtf8(isolate, script.data(),
          v8::NewStringType::kNormal, static_cast<int>(script.size())).ToLocalChecked();
      if (!v8::Script::Compile(context, source).ToLocal(&program) ||
          program->Run(context).IsEmpty()) {
        v8::String::Utf8Value error(isolate, errors.Exception());
        std::cerr << "public compiled-module review failed: "
                  << std::string(*error ? *error : "unknown", *error ?
                      std::min(error.length(), 4096) : 7) << '\n';
        status = 1;
      }
    }
    isolate->Dispose();
    v8::V8::Dispose();
    v8::V8::DisposePlatform();
    return status;
  } catch (const std::exception& error) {
    std::cerr << error.what() << '\n';
    return 1;
  }
}

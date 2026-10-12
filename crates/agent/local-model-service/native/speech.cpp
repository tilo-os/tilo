#include "whisper.h"
#include <algorithm>
#include <atomic>
#include <array>
#include <cstdint>
#include <cstring>
#include <cerrno>
#include <stdexcept>
#include <unistd.h>
#include <limits>
#include <new>
#include <string>
#include <signal.h>

// Only this confined child receives the parent's Finish hint. The signal
// handler performs one lock-free store; it never allocates, logs or touches
// model/audio state. The flag stays set until the process retires; finals do
// not install an abort callback and remain mandatory, complete decodes.
static std::atomic<bool> finish_requested{false};
static_assert(std::atomic<bool>::is_always_lock_free);
static void request_finish(int) { finish_requested.store(true, std::memory_order_relaxed); }

extern "C" bool tilo_speech_enable_draft_interrupt() noexcept {
    struct sigaction action{};
    action.sa_handler = request_finish;
    action.sa_flags = SA_RESTART;
    sigset_t signals;
    if (::sigemptyset(&action.sa_mask) != 0 || ::sigemptyset(&signals) != 0
        || ::sigaddset(&signals, SIGUSR1) != 0) return false;
    finish_requested.store(false, std::memory_order_relaxed);
    return ::sigaction(SIGUSR1, &action, nullptr) == 0
        && ::sigprocmask(SIG_UNBLOCK, &signals, nullptr) == 0;
}

// Reviewed private entry point: only a pure abort poll can preserve the
// already encoded full-context auto-language window with callbacks installed.
extern "C" int tilo_whisper_full_interruptible(whisper_context *, whisper_state *,
    whisper_full_params, const float *, int);

// This ABI lives only in the owned speech worker. No model-controlled message
// reaches diagnostics, and no closure or Rust userdata crosses a C callback.
static void discard_log(enum ggml_log_level, const char *, void *) {}

// The pinned GGML header identifies the reviewed base/small architectures. This
// chooses its allocation strategy only: the complete native loader still
// parses the receipt-checked immutable input. Tiny retains its measured
// non-flash path; no model, artifact-size or address-space admission changes.
static bool bounded_cpu_flash(const uint8_t * header, size_t length) noexcept {
    if (!header || length < 40) return false;
    auto word = [header](size_t offset) -> uint32_t {
        return uint32_t(header[offset]) | (uint32_t(header[offset + 1]) << 8)
            | (uint32_t(header[offset + 2]) << 16) | (uint32_t(header[offset + 3]) << 24);
    };
    // Head counts precede layer counts in the pinned GGML header.
    const bool base = word(12) == 512 && word(16) == 8 && word(20) == 6
        && word(28) == 512 && word(32) == 8 && word(36) == 6;
    const bool small = word(12) == 768 && word(16) == 12 && word(20) == 12
        && word(28) == 768 && word(32) == 12 && word(36) == 12;
    return word(0) == 0x67676d6c && (base || small);
}

static bool bounded_cpu_flash_fd(int fd) noexcept {
    std::array<uint8_t, 40> header{};
    size_t offset = 0;
    while (offset < header.size()) {
        const auto count = ::pread(fd, header.data() + offset, header.size() - offset,
                                   static_cast<off_t>(offset));
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) return false;
        offset += static_cast<size_t>(count);
    }
    return bounded_cpu_flash(header.data(), header.size());
}

struct PrivateTokens {
    std::array<whisper_token, 64> tokens{};
    int count = 0;
    void clear() noexcept {
        volatile whisper_token * data = tokens.data();
        for (size_t i = 0; i < tokens.size(); ++i) data[i] = 0;
        count = 0;
    }
    ~PrivateTokens() { clear(); }
};

struct PrivateWord {
    std::array<char, 514> text{};
    std::array<whisper_token, 64> tokens{};
    ~PrivateWord() {
        volatile char * bytes = text.data();
        for (size_t i = 0; i < text.size(); ++i) bytes[i] = 0;
        volatile whisper_token * ids = tokens.data();
        for (size_t i = 0; i < tokens.size(); ++i) ids[i] = 0;
    }
};

struct SpeechContext {
    whisper_context * model;
    whisper_state * state;
    int threads;
    bool flash_attention;
    bool pristine_state;
    bool draft_mode = false;
    PrivateTokens vocabulary{};
};

extern "C" void * tilo_speech_load(uint8_t * bytes, size_t length) noexcept try {
    whisper_log_set(discard_log, nullptr);
    auto params = whisper_context_default_params();
    params.use_gpu = false;
    params.flash_attn = bounded_cpu_flash(bytes, length);
    // Copy the weights first. The verified input buffer is released by Rust
    // before decoder arenas are allocated, avoiding an unnecessary peak.
    auto * model = whisper_init_from_buffer_with_params_no_state(bytes, length, params);
    if (!model) return nullptr;
    auto * context = new (std::nothrow) SpeechContext{model, nullptr, 2, params.flash_attn, false};
    if (!context) whisper_free(model);
    return context;
} catch (...) {
    // Native allocation/model exceptions must never unwind across the Rust ABI.
    // The confined worker reports a typed failure and exits; no automatic retry.
    return nullptr;
}

// Descriptor callbacks remain entirely in C++. The caller owns an immutable
// sealed file, retained only for this synchronous parse; no path is reopened.
extern "C" void * tilo_speech_load_fd(int fd, size_t length) noexcept try {
    if (fd < 0 || length < 64 || length > 256 * 1024 * 1024) return nullptr;
    struct Reader { int fd; size_t length; size_t offset; } reader{fd, length, 0};
    whisper_model_loader loader{};
    loader.context = &reader;
    loader.read = [](void * raw, void * output, size_t count) -> size_t {
        auto & input = *static_cast<Reader *>(raw);
        // The pinned parser probes three header fields at exact EOF before
        // checking eof(). Return initialized empty reads there; crossing EOF
        // inside an actual field or tensor still fails closed.
        if (input.offset == input.length) {
            if (count) std::memset(output, 0, count);
            return 0;
        }
        if (count > input.length - input.offset)
            throw std::runtime_error("speech descriptor bounds");
        size_t copied = 0;
        while (copied < count) {
            const auto read = ::pread(input.fd, static_cast<uint8_t *>(output) + copied,
                count - copied, static_cast<off_t>(input.offset + copied));
            if (read < 0 && errno == EINTR) continue;
            if (read <= 0) throw std::runtime_error("speech descriptor read");
            copied += static_cast<size_t>(read);
        }
        input.offset += copied;
        return copied;
    };
    loader.eof = [](void * raw) {
        const auto & input = *static_cast<Reader *>(raw);
        return input.offset == input.length;
    };
    loader.close = [](void *) {};
    whisper_log_set(discard_log, nullptr);
    auto params = whisper_context_default_params();
    params.use_gpu = false;
    params.flash_attn = bounded_cpu_flash_fd(fd);
    auto * model = whisper_init_with_params_no_state(&loader, params);
    if (!model) return nullptr;
    auto * context = new (std::nothrow) SpeechContext{model, nullptr, 2, params.flash_attn, false};
    if (!context) whisper_free(model);
    return context;
} catch (...) { return nullptr; }

extern "C" bool tilo_speech_flash_attention(void * context) noexcept {
    const auto * ctx = static_cast<const SpeechContext *>(context);
    return ctx && ctx->flash_attention;
}

extern "C" bool tilo_speech_threads(void * context, int threads) noexcept {
    auto * ctx = static_cast<SpeechContext *>(context);
    if (!ctx || threads < 1 || threads > 2) return false;
    ctx->threads = threads;
    return true;
}

extern "C" void tilo_speech_draft_mode(void * context, bool enabled) noexcept {
    auto * ctx = static_cast<SpeechContext *>(context);
    if (ctx) ctx->draft_mode = enabled;
}

// Vocabulary is data on the existing private pipe, never a model prompt from
// a page. Retain at most64 ordinary BPE tokens, ending at whole-word boundaries.
// Every call clears the previous binding, including a failed replacement.
extern "C" bool tilo_speech_vocabulary(void * context, const char * text, size_t length) noexcept try {
    auto * ctx = static_cast<SpeechContext *>(context);
    if (!ctx) return false;
    ctx->vocabulary.clear();
    if (!text || length > 16384) return false;
    if (length == 0) return true;
    const int budget = std::min(64, whisper_n_text_ctx(ctx->model)/2 - 1);
    if (budget <= 0) return false;
    PrivateTokens selected;
    size_t offset = 0;
    while (offset < length) {
        size_t end = offset;
        while (end < length && text[end] != '\n') ++end;
        const size_t size = end - offset;
        if (size == 0 || size > 512) return false;
        PrivateWord word;
        word.text[0] = ' ';
        std::memcpy(word.text.data()+1, text+offset, size);
        const int count = whisper_tokenize(ctx->model, word.text.data(), word.tokens.data(), 64);
        if (count <= 0 || count > budget - selected.count) break;
        for (int i = 0; i < count; ++i) {
            const auto token = word.tokens[static_cast<size_t>(i)];
            if (token < 0 || token >= whisper_token_eot(ctx->model)) return false;
            selected.tokens[static_cast<size_t>(selected.count++)] = token;
        }
        offset = end + 1;
    }
    if (selected.count == 0) return false;
    ctx->vocabulary.tokens = selected.tokens;
    ctx->vocabulary.count = selected.count;
    return true;
} catch (...) { return false; }

extern "C" bool tilo_speech_ready(void * context) noexcept try {
    auto * ctx = static_cast<SpeechContext *>(context);
    if (!ctx) return false;
    if (!ctx->state) {
        ctx->state = whisper_init_state(ctx->model);
        ctx->pristine_state = ctx->state != nullptr;
    }
    return ctx->state != nullptr;
} catch (...) {
    // Native allocation/model exceptions must never unwind across the Rust ABI.
    // The confined worker reports a typed failure and exits; no automatic retry.
    return false;
}

extern "C" void tilo_speech_free(void * context) noexcept {
    auto * ctx = static_cast<SpeechContext *>(context);
    if (!ctx) return;
    if (ctx->state) whisper_free_state(ctx->state);
    whisper_free(ctx->model);
    delete ctx;
}

extern "C" bool tilo_speech_accepts(void * context, const char * language) noexcept try {
    if (std::strcmp(language, "auto") == 0) return true;
    const int id = whisper_lang_id(language);
    auto * handle = static_cast<SpeechContext *>(context);
    if (!handle) return false;
    auto * ctx = handle->model;
    if (!whisper_is_multilingual(ctx)) return id == 0;
    // Pinned 1.8.3 vocabulary::num_languages; newer language IDs must not be
    // offered on older models whose vocabulary has no token for them.
    const int languages = whisper_model_n_vocab(ctx) - 51766;
    return id >= 0 && id < languages;
} catch (...) {
    // Native allocation/model exceptions must never unwind across the Rust ABI.
    // The confined worker reports a typed failure and exits; no automatic retry.
    return false;
}

// Returns a UTF-8 byte count, or a typed negative failure. Model parameters
// stay in C++ so Rust never assumes the layout of whisper_full_params.
extern "C" int tilo_speech_transcribe(void * context, const float * samples,
                                    int count, const char * language,
                                    uint8_t * output, size_t capacity) noexcept try {
    if (!context || count < 0 || count > 16000 * 120 || capacity > 16384) return -1;
    auto * handle = static_cast<SpeechContext *>(context);
    auto * ctx = handle->model;
    if (!tilo_speech_accepts(context, language)) return -2;
    if (handle->draft_mode && finish_requested.load(std::memory_order_relaxed)) return -5;
    // Readiness already allocated a pristine arena. Consume it once rather
    // than immediately discarding it. Every later inference still allocates
    // fresh state: fallback RNG, language detection and tokens never carry
    // across drafts/finals. Consume the marker before preprocessing so even
    // a failed inference cannot reuse partly mutated decoder state.
    if (handle->state && !handle->pristine_state) {
        whisper_free_state(handle->state);
        handle->state = nullptr;
    }
    if (!tilo_speech_ready(context)) return -3;
    handle->pristine_state = false;
    auto params = whisper_full_default_params(WHISPER_SAMPLING_GREEDY);
    // Greedy fallback defaults to five candidates and a sevenfold KV arena.
    // Keep one decoder within the worker budget even on uncertain speech.
    params.greedy.best_of = 1;
    params.n_threads = handle->threads;
    params.translate = false;
    params.no_context = true;
    params.print_special = false;
    params.print_progress = false;
    params.print_realtime = false;
    params.print_timestamps = false;
    params.suppress_blank = true;
    params.suppress_nst = true;
    params.language = language;
    if (handle->draft_mode) {
        params.abort_callback = [](void *) {
            return finish_requested.load(std::memory_order_relaxed);
        };
    }
    if (handle->vocabulary.count > 0) {
        params.prompt_tokens = handle->vocabulary.tokens.data();
        params.prompt_n_tokens = handle->vocabulary.count;
        params.n_max_text_ctx = 65;
        params.carry_initial_prompt = true;
    }
    // FFT preprocessing allocates per thread. An allocation exception while a
    // worker is joinable terminates before this ABI catch can run. Compute mel
    // serially inside the catch, then retain two threads for recognition. The
    // pinned API consumes precomputed mel when no new samples are supplied.
    if (count > 0 && whisper_pcm_to_mel_with_state(ctx, handle->state, samples, count, 1) != 0)
        return -3;
    if (handle->draft_mode && finish_requested.load(std::memory_order_relaxed)) return -5;
    const int result = handle->draft_mode
        ? tilo_whisper_full_interruptible(ctx, handle->state, params, nullptr, 0)
        : whisper_full_with_state(ctx, handle->state, params, nullptr, 0);
    // Pinned 1.8.3 frees the state when KV allocation fails (-7). Never leave
    // an owning handle pointing at it for destruction or a later call.
    if (result == -7) handle->state = nullptr;
    if (handle->draft_mode && finish_requested.load(std::memory_order_relaxed)) return -5;
    if (result != 0) return -3;
    std::string text;
    for (int index = 0; index < whisper_full_n_segments_from_state(handle->state); ++index) {
        const char * segment = whisper_full_get_segment_text_from_state(handle->state, index);
        if (!segment) return -3;
        const size_t size = std::strlen(segment);
        if (size > capacity - text.size()) return -4;
        text.append(segment, size);
    }
    std::memcpy(output, text.data(), text.size());
    return static_cast<int>(text.size());
} catch (...) {
    // Native allocation/model exceptions must never unwind across the Rust ABI.
    // The confined worker reports a typed failure and exits; no automatic retry.
    return -3;
}

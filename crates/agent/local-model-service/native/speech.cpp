#include "whisper.h"
#include <cstdint>
#include <cstring>
#include <limits>
#include <string>

// This ABI lives only in the owned speech worker. No model-controlled message
// reaches diagnostics, and no closure or Rust userdata crosses a C callback.
static void discard_log(enum ggml_log_level, const char *, void *) {}

extern "C" void * tilo_speech_load(uint8_t * bytes, size_t length) noexcept {
    whisper_log_set(discard_log, nullptr);
    auto params = whisper_context_default_params();
    params.use_gpu = false;
    params.flash_attn = false;
    return whisper_init_from_buffer_with_params(bytes, length, params);
}

extern "C" void tilo_speech_free(void * context) noexcept {
    whisper_free(static_cast<whisper_context *>(context));
}

extern "C" bool tilo_speech_accepts(void * context, const char * language) noexcept {
    if (std::strcmp(language, "auto") == 0) return true;
    const int id = whisper_lang_id(language);
    auto * ctx = static_cast<whisper_context *>(context);
    if (!whisper_is_multilingual(ctx)) return id == 0;
    // Pinned 1.8.3 vocabulary::num_languages; newer language IDs must not be
    // offered on older models whose vocabulary has no token for them.
    const int languages = whisper_model_n_vocab(ctx) - 51766;
    return id >= 0 && id < languages;
}

// Returns a UTF-8 byte count, or a typed negative failure. Model parameters
// stay in C++ so Rust never assumes the layout of whisper_full_params.
extern "C" int tilo_speech_transcribe(void * context, const float * samples,
                                    int count, const char * language,
                                    uint8_t * output, size_t capacity) noexcept {
    if (!context || count < 0 || count > 16000 * 120 || capacity > 16384) return -1;
    auto * ctx = static_cast<whisper_context *>(context);
    if (!tilo_speech_accepts(ctx, language)) return -2;
    auto params = whisper_full_default_params(WHISPER_SAMPLING_GREEDY);
    params.n_threads = 2;
    params.translate = false;
    params.no_context = true;
    params.print_special = false;
    params.print_progress = false;
    params.print_realtime = false;
    params.print_timestamps = false;
    params.suppress_blank = true;
    params.suppress_nst = true;
    params.language = language;
    if (whisper_full(ctx, params, samples, count) != 0) return -3;
    std::string text;
    for (int index = 0; index < whisper_full_n_segments(ctx); ++index) {
        const char * segment = whisper_full_get_segment_text(ctx, index);
        if (!segment) return -3;
        const size_t size = std::strlen(segment);
        if (size > capacity - text.size()) return -4;
        text.append(segment, size);
    }
    std::memcpy(output, text.data(), text.size());
    return static_cast<int>(text.size());
}

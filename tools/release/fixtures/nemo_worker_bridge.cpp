// SPDX-License-Identifier: Apache-2.0
// Private review worker ABI. No path opens, Rust callbacks or confidence policy.
#include "recognizer.h"
#include "runner.h"
#include <algorithm>
#include <cmath>
#include <cstring>
#include <memory>
#include <string>
#include <vector>

namespace {
constexpr size_t max_text = 16384;
constexpr size_t max_samples = 120 * 16000;
void discard_log(ggml_log_level, const char*, void*) {}
void wipe(std::string& text) noexcept {
    volatile char* bytes = text.empty() ? nullptr : &text[0];
    for (size_t i = 0; i < text.size(); ++i) bytes[i] = 0;
    text.clear();
}
struct Worker {
    std::unique_ptr<nemo_speech::asr::Recognizer> recognizer;
    std::unique_ptr<nemo_speech::asr::RecognitionStream> stream;
    std::string confirmed;
    std::string draft;
    size_t samples = 0;
    bool finished = false;
    ~Worker() { wipe(confirmed); wipe(draft); }
    void append(const std::string& text, bool space) {
        const size_t separator = space && !confirmed.empty() && !text.empty();
        if (text.size() > max_text || confirmed.size() > max_text - text.size() ||
            separator > max_text - confirmed.size() - text.size())
            throw std::length_error("text bound");
        if (separator) confirmed += ' ';
        confirmed += text;
    }
    void accept(const nemo_speech::asr::Result& result) {
        append(result.late_punctuation, false);
        const std::string text = result.alternatives.empty() ? "" : result.alternatives.front().transcript;
        if (text.size() > max_text) throw std::length_error("text bound");
        wipe(draft);
        if (result.is_final) append(text, true);
        else draft = text;
    }
    int output(char* bytes, size_t capacity) const {
        const size_t separator = !confirmed.empty() && !draft.empty();
        const size_t length = confirmed.size() + separator + draft.size();
        if (length > max_text || length > capacity) return -4;
        memcpy(bytes, confirmed.data(), confirmed.size());
        if (separator) bytes[confirmed.size()] = ' ';
        memcpy(bytes + confirmed.size() + separator, draft.data(), draft.size());
        return static_cast<int>(length);
    }
};

int request_options(Worker& worker, const char* input_language, const char* input_vocabulary,
                    nemo_speech::asr::AsrRequestOptions& options, std::string& language) {
    if (!input_language || !input_vocabulary) return -3;
    const auto languages = worker.recognizer->supported_languages();
    language = input_language;
    const std::vector<std::pair<std::string, std::string>> aliases = {
        {"en", "en-US"}, {"de", "de-DE"}, {"fr", "fr-FR"},
        {"es", "es-ES"}, {"ar", "ar-AR"}, {"zh", "zh-CN"}};
    // Respect the model's declared code before applying a fixed fallback.
    // In this pinned model, "ar" and "es" are valid prompt entries themselves.
    if (std::find(languages.begin(), languages.end(), language) == languages.end()) {
        for (const auto& alias : aliases) if (language == alias.first) language = alias.second;
    }
    if (std::find(languages.begin(), languages.end(), language) == languages.end()) {
        return -2;
    }
    options.language_code = language;
    options.verbatim_transcripts = true; // no unavailable native ITN grammars
    options.enable_automatic_punctuation = true;
    nemo_speech::asr::AsrRequestOptions::Boost context;
    context.boost = 1.0f;
    std::string vocabulary = input_vocabulary;
    if (!vocabulary.empty()) {
        auto* model = worker.recognizer->model();
        if (model->head_kind() == nemo_speech::asr::HeadKind::Ctc) return -3;
        auto* rnnt = static_cast<nemo_speech::asr::RnntModel*>(model);
        size_t tokens = 0;
        for (size_t start = 0; start < vocabulary.size();) {
            size_t end = vocabulary.find('\n', start);
            if (end == std::string::npos) end = vocabulary.size();
            const std::string phrase = vocabulary.substr(start, end - start);
            if (phrase.empty()) return -3;
            const auto ids = rnnt->encode_phrase(phrase);
            if (ids.empty() || ids.size() > 64 || tokens > 64 - ids.size()) return -3;
            tokens += ids.size();
            context.phrases.push_back(phrase);
            start = end + 1;
        }
        options.speech_contexts.push_back(std::move(context));
    }
    wipe(vocabulary);
    return 0;
}
}

struct TiloNemoConfig {
    int descriptor;
    int threads;
    int right_context;
    const char* language;
    const char* vocabulary; // validated newline-separated accepted spellings
};

extern "C" __attribute__((visibility("default"))) void* tilo_nemo_load(const TiloNemoConfig* input, int* status) noexcept {
    *status = -3;
    try {
        if (!input || input->descriptor < 0 || input->threads < 1 || input->threads > 2 ||
            !input->language || !input->vocabulary ||
            (input->right_context != 0 && input->right_context != 1 &&
             input->right_context != 3)) return nullptr;
        ggml_log_set(discard_log, nullptr);
        nemo_speech::asr::RecognizerConfig config;
        config.model.descriptor = input->descriptor;
        config.backend.gpu = -1;
        config.backend.threads = input->threads;
        config.streaming.rnnt_right_context = input->right_context;
        config.postproc.cpu_workers = 1;
        config.log_status = false;
        config.batching.enabled = false;
        config.batching.state_arena_slots = 1;
        config.decoder.boosting_tree_alpha = 1.0;
        auto worker = std::make_unique<Worker>();
        worker->recognizer = std::make_unique<nemo_speech::asr::Recognizer>(config);
        nemo_speech::asr::AsrRequestOptions options;
        std::string language;
        *status = request_options(*worker, input->language, input->vocabulary, options, language);
        if (*status != 0) return nullptr;
        // Prepare one actual cache-aware inference shape before Ready. The
        // server warmup repeats four seconds of silence; one bounded chunk
        // plus finalize loads lazy weights and graphs without that repeated
        // work. Destroy all synthetic stream state before the user's stream.
        auto warmup = worker->recognizer->make_runner();
        auto* model = worker->recognizer->model();
        if (model->has_prompt())
            warmup->set_prompt_index(model->prompt_index_for_lang("auto"));
        if (worker->recognizer->sample_rate() != 16000)
            throw std::invalid_argument("sample rate");
        std::vector<float> silence(1600, 0.0f);
        warmup->feed_audio(silence.data(), silence.size());
        warmup->step();
        warmup->finalize();
        warmup.reset();
        worker->stream = worker->recognizer->streaming_recognize(options, language, false);
        if (!worker->stream) return nullptr;
        *status = 0;
        return worker.release();
    } catch (...) { return nullptr; }
}

extern "C" __attribute__((visibility("default"))) int tilo_nemo_push(void* pointer, const float* samples, size_t count) noexcept {
    try {
        auto& worker = *static_cast<Worker*>(pointer);
        if (worker.finished || count == 0 || count > max_samples - worker.samples) return -3;
        for (size_t i = 0; i < count; ++i) if (!std::isfinite(samples[i])) return -3;
        worker.stream->push(samples, count, 16000);
        worker.samples += count;
        return 0;
    } catch (...) { return -3; }
}

extern "C" __attribute__((visibility("default"))) int tilo_nemo_text(void* pointer, bool complete, char* bytes, size_t capacity) noexcept {
    auto& worker = *static_cast<Worker*>(pointer);
    try {
        if (worker.finished) return -3;
        if (complete) {
            worker.finished = true;
            worker.accept(worker.stream->finish());
        } else {
            while (auto result = worker.stream->next()) {
                worker.accept(*result);
                if (!result->is_final) break;
            }
        }
        return worker.output(bytes, capacity);
    } catch (const std::length_error&) { worker.finished = true; return -4; }
      catch (...) { worker.finished = true; return -3; }
}

extern "C" __attribute__((visibility("default"))) void tilo_nemo_free(void* pointer) noexcept {
    delete static_cast<Worker*>(pointer);
}

extern "C" __attribute__((visibility("default"))) int tilo_nemo_reset(
    void* pointer, const char* language, const char* vocabulary) noexcept {
    try {
        auto& worker = *static_cast<Worker*>(pointer);
        // Only fully finished sessions may reuse immutable model resources.
        // Cancellation still kills the owned process; no active input is reused.
        if (!worker.finished) return -3;
        nemo_speech::asr::AsrRequestOptions options;
        std::string resolved;
        const int status = request_options(worker, language, vocabulary, options, resolved);
        if (status != 0) return status;
        // Free the old decoder slot before acquiring the one supported slot.
        worker.stream.reset();
        wipe(worker.confirmed);
        wipe(worker.draft);
        worker.samples = 0;
        worker.stream = worker.recognizer->streaming_recognize(options, resolved, false);
        if (!worker.stream) return -3;
        worker.finished = false;
        return 0;
    } catch (...) { return -3; }
}

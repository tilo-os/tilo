// Public-audio experimental adapter against the exact reviewed native C++ API.
// The pinned C ABI omits CPU thread configuration; this adapter sets it before
// model/stream creation. No confidence score is read or used.
#include "recognizer.h"
#include "wav_reader.h"
#include <algorithm>
#include <chrono>
#include <cstdio>
#include <string>
#include <thread>
#include <vector>

static double elapsed(std::chrono::steady_clock::time_point start) {
    return std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - start).count();
}
static std::string json_text(const std::string& input) {
    std::string out = "\"";
    for (unsigned char c : input) {
        if (c == '"' || c == '\\') { out += '\\'; out += c; }
        else if (c < 32) { char escaped[7]; std::snprintf(escaped, sizeof(escaped), "\\u%04x", c); out += escaped; }
        else out += c;
    }
    return out + '"';
}
static std::string transcript(const nemo_speech::asr::Result& result) {
    return result.alternatives.empty() ? std::string() : result.alternatives[0].transcript;
}
int main(int argc, char** argv) try {
    if (argc != 6) return 2;
    const int threads = std::stoi(argv[4]);
    if (threads < 1 || threads > 2) return 2;
    const bool paced = std::string(argv[5]) == "paced";
    if (!paced && std::string(argv[5]) != "offline") return 2;
    std::vector<float> audio;
    std::string error;
    if (!examples::read_wav_mono_16k(argv[2], audio, error) || audio.empty() || audio.size() > 120 * 16000) return 2;
    nemo_speech::asr::RecognizerConfig config;
    config.model.path = argv[1];
    config.backend.gpu = -1;
    config.backend.threads = threads;
    config.postproc.cpu_workers = 1;
    config.log_status = false;
    config.batching.enabled = false;
    const auto load_started = std::chrono::steady_clock::now();
    nemo_speech::asr::Recognizer recognizer(config);
    const std::string requested_language = argv[3];
    const auto supported_languages = recognizer.supported_languages();
    std::printf("ASR_LANGUAGE {\"requested\":%s,\"supported\":[", json_text(requested_language).c_str());
    for (size_t i = 0; i < supported_languages.size(); ++i) {
        if (i) std::putchar(',');
        std::fputs(json_text(supported_languages[i]).c_str(), stdout);
    }
    std::puts("]}"); std::fflush(stdout);
    // The pinned resolver silently falls back to auto for unknown tags.
    // A selected-language comparison must fail rather than report that fallback.
    if (std::find(supported_languages.begin(), supported_languages.end(), requested_language)
            == supported_languages.end()) {
        std::fputs("ASR_ERROR unsupported_language\n", stderr);
        return 2;
    }
    nemo_speech::asr::AsrRequestOptions options;
    options.language_code = std::string(argv[3]) == "auto" ? "" : argv[3];
    options.verbatim_transcripts = true;
    options.enable_automatic_punctuation = true;
    std::printf("ASR_LOAD {\"cold_ms\":%.3f,\"threads\":%d,\"postprocessing_workers\":1}\n", elapsed(load_started), threads);
    std::fflush(stdout);
    const auto capture = std::chrono::steady_clock::now();
    std::string final;
    double final_ms = 0;
    if (paced) {
        auto stream = recognizer.streaming_recognize(options, options.language_code, false);
        size_t sent = 0;
        while (sent < audio.size()) {
            // Preserve the source clock and retain backlog. Each 160 ms chunk
            // is presented once, never repeated or accelerated to hide latency.
            const size_t end = std::min(audio.size(), sent + 2560);
            std::this_thread::sleep_until(capture + std::chrono::duration_cast<std::chrono::steady_clock::duration>(std::chrono::duration<double>(end / 16000.0)));
            stream->push(audio.data() + sent, end - sent, 16000);
            sent = end;
            const auto inference = std::chrono::steady_clock::now();
            while (auto result = stream->next()) {
                std::printf("ASR_DRAFT {\"samples\":%zu,\"wall_ms\":%.3f,\"inference_ms\":%.3f,\"audio_processed_seconds\":%.3f,\"text\":%s}\n", sent, elapsed(capture), elapsed(inference), result->audio_processed, json_text(transcript(*result)).c_str());
                std::fflush(stdout);
                if (!result->is_final) break;
            }
        }
        const auto stop = std::chrono::steady_clock::now();
        final = transcript(stream->finish());
        final_ms = elapsed(stop);
    } else {
        const auto stop = std::chrono::steady_clock::now();
        final = transcript(recognizer.recognize(audio.data(), audio.size(), options, options.language_code, 16000));
        final_ms = elapsed(stop);
    }
    std::printf("ASR_FINAL {\"samples\":%zu,\"finalization_ms\":%.3f,\"source_final_ms\":%.3f,\"text\":%s}\n", audio.size(), final_ms, elapsed(capture), json_text(final).c_str());
    return 0;
} catch (...) {
    std::fputs("ASR_ERROR native_runtime\n", stderr);
    return 3;
}

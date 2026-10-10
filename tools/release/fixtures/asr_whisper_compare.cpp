// Public, bounded experimental adapter over the unchanged production speech ABI.
// No microphone, network, catalogue, trust or installation path exists here.
#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <fstream>
#include <iterator>
#include <string>
#include <thread>
#include <vector>

extern "C" void* tilo_speech_load(uint8_t*, size_t) noexcept;
extern "C" bool tilo_speech_threads(void*, int) noexcept;
extern "C" bool tilo_speech_ready(void*) noexcept;
extern "C" int tilo_speech_transcribe(void*, const float*, int, const char*, uint8_t*, size_t) noexcept;
extern "C" void tilo_speech_free(void*) noexcept;

static std::string json_text(const std::string& input) {
    std::string out = "\"";
    for (unsigned char c : input) {
        if (c == '"' || c == '\\') { out += '\\'; out += c; }
        else if (c < 32) { char escaped[7]; std::snprintf(escaped, sizeof(escaped), "\\u%04x", c); out += escaped; }
        else out += c;
    }
    return out + '"';
}
static double elapsed(std::chrono::steady_clock::time_point start) {
    return std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - start).count();
}
static bool decode(void* model, const std::vector<float>& audio, size_t samples,
                   const char* language, std::string& text) {
    std::vector<uint8_t> result(16384);
    const int size = tilo_speech_transcribe(model, audio.data(), static_cast<int>(samples), language,
                                           result.data(), result.size());
    if (size < 0) return false;
    text.assign(reinterpret_cast<const char*>(result.data()), static_cast<size_t>(size));
    return true;
}
int main(int argc, char** argv) {
    if (argc != 6) return 2; // model, canonical PCM16 WAV, language, threads, offline|paced
    const int threads = std::stoi(argv[4]);
    if (threads < 1 || threads > 2) return 2;
    const bool paced = std::string(argv[5]) == "paced";
    if (!paced && std::string(argv[5]) != "offline") return 2;
    std::ifstream input(argv[2], std::ios::binary);
    std::vector<uint8_t> wav((std::istreambuf_iterator<char>(input)), {});
    if (wav.size() < 44 || wav.size() > 120 * 32000 + 44 || wav[0] != 'R' || wav[36] != 'd') return 2;
    std::vector<float> audio;
    for (size_t i = 44; i + 1 < wav.size(); i += 2) {
        const auto value = static_cast<int16_t>(static_cast<uint16_t>(wav[i]) | (static_cast<uint16_t>(wav[i+1]) << 8));
        audio.push_back(value / 32768.0f);
    }
    std::ifstream weights(argv[1], std::ios::binary | std::ios::ate);
    const auto size = weights.tellg();
    if (size <= 0 || size > 256 * 1024 * 1024) return 2;
    weights.seekg(0);
    std::vector<uint8_t> bytes(static_cast<size_t>(size));
    if (!weights.read(reinterpret_cast<char*>(bytes.data()), size)) return 2;
    const auto started = std::chrono::steady_clock::now();
    void* model = tilo_speech_load(bytes.data(), bytes.size());
    bytes.clear(); bytes.shrink_to_fit();
    if (!model || !tilo_speech_threads(model, threads) || !tilo_speech_ready(model)) return 3;
    std::printf("ASR_LOAD {\"cold_ms\":%.3f}\n", elapsed(started)); std::fflush(stdout);
    const auto capture = std::chrono::steady_clock::now();
    size_t sent = 0;
    std::string text;
    if (paced) {
        while (sent < audio.size()) {
            const double next = sent / 16000.0 + (sent == 0 ? 2.0 : 1.0);
            std::this_thread::sleep_until(capture + std::chrono::duration_cast<std::chrono::steady_clock::duration>(std::chrono::duration<double>(next)));
            sent = std::min(audio.size(), static_cast<size_t>(elapsed(capture) * 16));
            const auto inference = std::chrono::steady_clock::now();
            if (!decode(model, audio, sent, argv[3], text)) { tilo_speech_free(model); return 4; }
            std::printf("ASR_DRAFT {\"samples\":%zu,\"wall_ms\":%.3f,\"inference_ms\":%.3f,\"text\":%s}\n", sent, elapsed(capture), elapsed(inference), json_text(text).c_str());
            std::fflush(stdout);
        }
    }
    const auto final_started = std::chrono::steady_clock::now();
    // Paced unchanged-input final mirrors the production recognition cache.
    if (!paced && !decode(model, audio, audio.size(), argv[3], text)) { tilo_speech_free(model); return 4; }
    std::printf("ASR_FINAL {\"samples\":%zu,\"finalization_ms\":%.3f,\"source_final_ms\":%.3f,\"text\":%s}\n", audio.size(), elapsed(final_started), elapsed(capture), json_text(text).c_str());
    tilo_speech_free(model);
    return 0;
}

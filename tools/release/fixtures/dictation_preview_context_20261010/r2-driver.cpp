// Review-only public-audio experiment; never linked into the production worker.
#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <fcntl.h>
#include <fstream>
#include <string>
#include <sys/stat.h>
#include <unistd.h>
#include <vector>

extern "C" void* tilo_speech_load_fd(int, size_t) noexcept;
extern "C" bool tilo_speech_threads(void*, int) noexcept;
extern "C" bool tilo_speech_ready(void*) noexcept;
extern "C" int tilo_speech_transcribe_context(void*, const float*, int, const char*, uint8_t*, size_t, int) noexcept;
extern "C" void tilo_speech_free(void*) noexcept;

static std::string json_text(const std::string& input) {
    std::string output = "\"";
    for (unsigned char c : input) {
        if (c == '"' || c == '\\') { output += '\\'; output += c; }
        else if (c < 32) {
            char escaped[7];
            std::snprintf(escaped, sizeof(escaped), "\\u%04x", c);
            output += escaped;
        } else output += c;
    }
    return output + '"';
}

static void memory() {
    std::ifstream status("/proc/self/status");
    std::string line;
    std::printf("PREVIEW_MEMORY {");
    bool first = true;
    while (std::getline(status, line)) {
        for (const char* key : {"VmPeak", "VmRSS", "VmHWM", "Threads"}) {
            const std::string prefix = std::string(key) + ":";
            if (line.rfind(prefix, 0) == 0) {
                std::printf("%s\"%s\":%lld", first ? "" : ",", key,
                            std::stoll(line.substr(prefix.size())));
                first = false;
            }
        }
    }
    std::puts("}");
}

int main(int argc, char** argv) {
    // Exact externally receipt-checked model, LE float32 PCM, selected language.
    if (argc != 4) return 2;
    std::ifstream pcm(argv[2], std::ios::binary | std::ios::ate);
    const auto bytes = static_cast<std::streamoff>(pcm.tellg());
    if (bytes <= 0 || bytes > 120 * 16000 * 4 || bytes % 4 != 0) return 2;
    pcm.seekg(0);
    std::vector<float> audio(static_cast<size_t>(bytes) / 4);
    if (!pcm.read(reinterpret_cast<char*>(audio.data()), bytes)) return 2;
    if (std::any_of(audio.begin(), audio.end(), [](float value) { return !std::isfinite(value); })) return 2;
    const int fd = ::open(argv[1], O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    struct stat info{};
    if (fd < 0 || ::fstat(fd, &info) != 0 || !S_ISREG(info.st_mode) || info.st_size <= 0) return 2;
    void* model = tilo_speech_load_fd(fd, static_cast<size_t>(info.st_size));
    ::close(fd);
    if (!model || !tilo_speech_threads(model, 2) || !tilo_speech_ready(model)) {
        if (model) tilo_speech_free(model);
        return 3;
    }
    std::vector<uint8_t> result(16384);
    // Paired identical prefixes, alternating order to expose warm-up bias.
    for (int seconds : {2, 4, 6}) {
        const size_t count = std::min(audio.size(), static_cast<size_t>(seconds * 16000));
        for (int candidate : {0, 1250}) {
            const int context = seconds == 4 ? (candidate == 0 ? 1250 : 0) : candidate;
            const auto started = std::chrono::steady_clock::now();
            const int length = tilo_speech_transcribe_context(model, audio.data(), static_cast<int>(count),
                argv[3], result.data(), result.size(), context);
            if (length < 0 || static_cast<size_t>(length) > result.size()) { tilo_speech_free(model); return 4; }
            const double elapsed = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - started).count();
            const std::string text(reinterpret_cast<char*>(result.data()), static_cast<size_t>(length));
            std::printf("PREVIEW_PREFIX {\"samples\":%zu,\"context\":%d,\"inference_ms\":%.3f,\"text\":%s}\n",
                        count, context, elapsed, json_text(text).c_str());
            memory();
            std::fflush(stdout);
        }
    }
    // Always actually decode the full-context final, even with unchanged audio.
    const auto final_started = std::chrono::steady_clock::now();
    const int length = tilo_speech_transcribe_context(model, audio.data(), static_cast<int>(audio.size()),
        argv[3], result.data(), result.size(), 0);
    if (length < 0 || static_cast<size_t>(length) > result.size()) { tilo_speech_free(model); return 4; }
    const double elapsed = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - final_started).count();
    std::printf("PREVIEW_FINAL {\"samples\":%zu,\"context\":0,\"inference_ms\":%.3f,\"text\":%s}\n",
        audio.size(), elapsed, json_text(std::string(reinterpret_cast<char*>(result.data()), static_cast<size_t>(length))).c_str());
    memory();
    tilo_speech_free(model);
    return 0;
}

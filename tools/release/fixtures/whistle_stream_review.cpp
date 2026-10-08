#include "needle.h"
#include <unistd.h>
__attribute__((constructor(101))) static void early_phase() {
 const char message[] = "WHISTLE_PHASE early_program_constructor\n";
 (void)write(STDOUT_FILENO, message, sizeof(message)-1);
}
#include <algorithm>
#include <array>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <dirent.h>
#include <fstream>
#include <iterator>
#include <memory>
#include <sstream>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>

// Prerecorded development adapter only. No microphone, network, downloader,
// signing, catalogue authority or production worker protocol is provided here.
using Clock = std::chrono::steady_clock;
static double ms(Clock::time_point start) {
    return std::chrono::duration<double, std::milli>(Clock::now() - start).count();
}
// Optional startup diagnostic only; the external admission guard is unchanged.
// No audio, transcripts or arbitrary process state are inspected here.
static void emit_startup_tasks() {
    const auto close_tasks = [](DIR * directory) { closedir(directory); };
    std::unique_ptr<DIR, decltype(close_tasks)> tasks(opendir("/proc/self/task"), close_tasks);
    if (!tasks) throw std::runtime_error("cannot observe owned native tasks");
    unsigned count = 0;
    while (auto * entry = readdir(tasks.get())) {
        const std::string id(entry->d_name);
        if (id.empty() || id.find_first_not_of("0123456789") != std::string::npos) continue;
        if (++count > 64) throw std::runtime_error("bounded owned task snapshot");
        const auto prefix = std::string("/proc/self/task/") + id + "/";
        std::ifstream stat(prefix + "stat");
        std::string line;
        if (!std::getline(stat, line) || line.rfind(')') == std::string::npos)
            throw std::runtime_error("cannot observe owned task accounting");
        std::istringstream fields(line.substr(line.rfind(')') + 1));
        char state = 0;
        fields >> state;
        std::string value;
        for (int field = 4; field <= 13; ++field) fields >> value;
        unsigned long long user_ticks = 0, system_ticks = 0;
        fields >> user_ticks >> system_ticks;
        std::ifstream sched(prefix + "schedstat"), wait(prefix + "wchan");
        unsigned long long runtime_ns = 0, delay_ns = 0, slices = 0;
        std::string wchan;
        if (!fields || !(sched >> runtime_ns >> delay_ns >> slices) || !(wait >> wchan)
            || wchan.size() > 80 || wchan.find_first_not_of("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_") != std::string::npos)
            throw std::runtime_error("cannot observe owned task scheduling");
        std::printf("WHISTLE_TASK {\"tid\":%s,\"state\":\"%c\",\"user_ticks\":%llu,\"system_ticks\":%llu,\"runtime_ns\":%llu,\"runqueue_delay_ns\":%llu,\"slices\":%llu,\"wchan\":\"%s\"}\n",
                    id.c_str(), state, user_ticks, system_ticks, runtime_ns, delay_ns, slices, wchan.c_str());
        std::fflush(stdout);
    }
    std::printf("WHISTLE_TASK_SNAPSHOT {\"tasks\":%u,\"clock_ticks_per_second\":%ld,\"online_cpus\":%ld}\n",
                count, sysconf(_SC_CLK_TCK), sysconf(_SC_NPROCESSORS_ONLN));
    std::fflush(stdout);
}
static void emit(const char * kind, int session, double wall, double inference,
                 const std::array<char, 65536> & output) {
    if (!std::memchr(output.data(), 0, output.size()))
        throw std::runtime_error("bounded native output");
    std::printf("WHISTLE_REPLY {\"kind\":\"%s\",\"session\":%d,\"wall_ms\":%.6f,\"inference_ms\":%.6f,\"result\":%s}\n",
                kind, session, wall, inference, output.data());
    std::fflush(stdout);
}
int main(int argc, char ** argv) try {
    std::puts("WHISTLE_PHASE main_entered"); std::fflush(stdout);
    if (argc != 4 && !(argc == 5 && std::strcmp(argv[4], "--startup-tasks") == 0)) return 2;
    const std::array<const char *, 7> languages{"en", "de", "fr", "es", "it", "nl", "pl"};
    if (std::none_of(languages.begin(), languages.end(), [&](const char * value) {
        return std::strcmp(value, argv[3]) == 0;
    })) return 2;
    std::ifstream weights(argv[1], std::ios::binary);
    std::vector<unsigned char> model((std::istreambuf_iterator<char>(weights)), {});
    if (model.size() != 16919407) return 3;
    std::ifstream input(argv[2], std::ios::binary | std::ios::ate);
    const auto length = input.tellg();
    if (length <= 0 || length > 30 * 16000 * 4 || length % 4) return 4;
    std::vector<float> audio(static_cast<size_t>(length) / 4);
    input.seekg(0);
    input.read(reinterpret_cast<char *>(audio.data()), length);
    if (!input || std::any_of(audio.begin(), audio.end(), [](float value) {
        return !std::isfinite(value) || value < -1 || value > 1;
    })) return 5;
    std::puts("WHISTLE_PHASE before_needle_load"); std::fflush(stdout);
    const auto loading = Clock::now();
    if (needle_load(model.data(), model.size()) < 0 || !(needle_models() & NEEDLE_SPEECH)) return 6;
    std::printf("WHISTLE_LOAD {\"native_ms\":%.6f}\n", ms(loading));
    std::fflush(stdout);
    if (argc == 5) emit_startup_tasks();
    std::array<char, 65536> output{};
    // Repeat in one actual native context, with a silence session in between.
    // Stop must reset the engine's stream rather than retain prior words.
    for (int session = 0; session < 3; ++session) {
        std::vector<float> silence(3 * 16000, 0.0f);
        const auto & pcm = session == 1 ? silence : audio;
        const auto start = Clock::now();
        size_t offset = 0;
        size_t planned = 32000; // same two-second opening as the Whisper control
        while (offset < pcm.size()) {
            planned = std::min(planned, pcm.size());
            std::this_thread::sleep_until(start + std::chrono::microseconds(planned * 1000000 / 16000));
            const auto elapsed_samples = static_cast<size_t>(ms(start) * 16);
            const auto end = std::min(pcm.size(), std::max(planned, elapsed_samples));
            output.fill(0);
            const auto inference = Clock::now();
            if (needle_stream_transcribe_process(pcm.data() + offset, static_cast<int>(end - offset), argv[3], nullptr, output.data(), output.size()) < 0) return 7;
            const auto inference_ms = ms(inference);
            emit("draft", session, ms(start), inference_ms, output);
            offset = end;
            planned = offset + 16000;
        }
        output.fill(0);
        const auto inference = Clock::now();
        if (needle_stream_transcribe_stop(output.data(), output.size()) < 0) return 8;
        const auto inference_ms = ms(inference);
        emit("done", session, ms(start), inference_ms, output);
    }
    return 0;
} catch (...) {
    // Native error strings and paths are not diagnostics.
    std::fputs("WHISTLE_REVIEW_FAILED\n", stderr);
    return 9;
}

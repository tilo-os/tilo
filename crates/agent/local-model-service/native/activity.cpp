// Acoustic evidence is independent of decoder confidence. Original PCM is never
// changed.
#include "whisper.h"
#include <algorithm>
#include <cerrno>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <stdexcept>
#include <unistd.h>
#include <vector>

struct PrivateAnalysis {
  std::vector<float> samples;
  ~PrivateAnalysis() {
    volatile float *data = samples.data();
    for (size_t i = 0; i < samples.size(); ++i)
      data[i] = 0;
  }
};
struct PrivateSilero {
  whisper_vad_context *context = nullptr;
  ~PrivateSilero() {
    if (context)
      whisper_vad_free(context);
  }
};

struct Reader {
  const uint8_t *bytes;
  size_t size;
  size_t offset;
};
static whisper_vad_context *load(const uint8_t *model, size_t length) {
  if (!model || length < 64 || length > 4 * 1024 * 1024)
    return nullptr;
  Reader reader{model, length, 0};
  whisper_model_loader loader{};
  loader.context = &reader;
  loader.read = [](void *data, void *out, size_t size) -> size_t {
    auto &input = *static_cast<Reader *>(data);
    if (input.offset == input.size) {
      if (size)
        std::memset(out, 0, size);
      return 0;
    }
    if (size > input.size - input.offset)
      throw std::runtime_error("VAD model bounds");
    std::memcpy(out, input.bytes + input.offset, size);
    input.offset += size;
    return size;
  };
  loader.eof = [](void *data) {
    auto &input = *static_cast<Reader *>(data);
    return input.offset == input.size;
  };
  loader.close = [](void *) {};
  whisper_log_set([](enum ggml_log_level, const char *, void *) {}, nullptr);
  auto params = whisper_vad_default_context_params();
  params.use_gpu = false;
  // This small per-frame graph needs no second idle/spinning CPU pool.
  params.n_threads = 1;
  return whisper_vad_init_with_params(&loader, params);
}

extern "C" void *tilo_activity_load_fd(int fd, size_t length) noexcept try {
  if (fd < 0 || length < 64 || length > 4 * 1024 * 1024)
    return nullptr;
  std::vector<uint8_t> bytes(length);
  size_t offset = 0;
  while (offset < length) {
    const auto count = ::pread(fd, bytes.data() + offset, length - offset,
                               static_cast<off_t>(offset));
    if (count < 0 && errno == EINTR)
      continue;
    if (count <= 0)
      return nullptr;
    offset += static_cast<size_t>(count);
  }
  return load(bytes.data(), bytes.size());
} catch (...) {
  return nullptr;
}
extern "C" void tilo_activity_free(void *context) noexcept {
  if (context)
    whisper_vad_free(static_cast<whisper_vad_context *>(context));
}
extern "C" bool tilo_activity_observe(void *context, const float *samples,
                                      int count, bool normalize,
                                      uint64_t *counters,
                                      float *probabilities) noexcept try {
  if (!context || !samples || count <= 0 || count > 16000 * 120 || !counters ||
      !probabilities)
    return false;
  float peak = 0;
  for (int i = 0; i < count; ++i) {
    if (!std::isfinite(samples[i]) || std::abs(samples[i]) > 1)
      return false;
    peak = std::max(peak, std::abs(samples[i]));
  }
  PrivateAnalysis analysis;
  if (normalize && peak > 0) {
    analysis.samples.resize(static_cast<size_t>(count));
    for (int i = 0; i < count; ++i)
      analysis.samples[static_cast<size_t>(i)] = samples[i] * (0.5f / peak);
    samples = analysis.samples.data();
  }
  if (!whisper_vad_detect_speech(static_cast<whisper_vad_context *>(context),
                                 samples, count))
    return false;
  const int frames =
      whisper_vad_n_probs(static_cast<whisper_vad_context *>(context));
  const float *probs =
      whisper_vad_probs(static_cast<whisper_vad_context *>(context));
  if (!probs || frames <= 0 || frames > 6000)
    return false;
  uint64_t positives = 0, first = 0, last = 0, longest = 0, consecutive = 0;
  float minimum = 1, maximum = 0, total = 0;
  for (int i = 0; i < frames; ++i) {
    if (!std::isfinite(probs[i]) || probs[i] < 0 || probs[i] > 1)
      return false;
    minimum = std::min(minimum, probs[i]);
    maximum = std::max(maximum, probs[i]);
    total += probs[i];
    if (probs[i] >= 0.5f) {
      ++positives;
      if (first == 0)
        first = static_cast<uint64_t>(i + 1);
      last = static_cast<uint64_t>(i + 1);
      longest = std::max(longest, ++consecutive);
    } else
      consecutive = 0;
  }
  *counters = positives | (first << 16) | (last << 32) | (longest << 48);
  probabilities[0] = minimum;
  probabilities[1] = maximum;
  probabilities[2] = total / frames;
  probabilities[3] = static_cast<float>(frames);
  return true;
} catch (...) {
  return false;
}

// Diagnostic caller verifies a pinned public graph; normal workers use sealed
// descriptors.
extern "C" bool tilo_silero_activity(const uint8_t *model, size_t length,
                                     const float *samples, int count,
                                     bool normalize, uint64_t *counters,
                                     float *probabilities) noexcept try {
  PrivateSilero owner;
  owner.context = load(model, length);
  return tilo_activity_observe(owner.context, samples, count, normalize,
                               counters, probabilities);
} catch (...) {
  return false;
}

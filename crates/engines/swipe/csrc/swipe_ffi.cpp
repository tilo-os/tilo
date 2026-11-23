// C ABI shim over the FUTO swipe-library `SwipeEngine` (GPL-3.0-or-later;
// linked into Tilo, which is AGPL-3.0-only — compatible). Compiled by
// build.rs via the `cc` crate and linked against libswipe_decoder.a +
// ExecuTorch static libs. The Rust side (`tilo-swipe`) calls these.
//
// The engine uses the library's built-in `default_en_qwerty()` layout
// (normalized [0,1] key centres); callers normalize their swipe path into
// that space. The dictionary is built from a FUTO `.combined` vocab file.

#include "swipe_decoder/engine.hpp"
#include "swipe_decoder/trie.hpp"
#include "swipe_decoder/layout.hpp"

#include <cstdint>
#include <cstring>
#include <optional>
#include <string>
#include <vector>

using swipe_decoder::DecodedWord;
using swipe_decoder::KeyboardLayout;
using swipe_decoder::SwipeEngine;
using swipe_decoder::Trie;

namespace {

struct Handle {
    // The Trie must outlive the engine (the engine holds an ITrie* into it).
    std::optional<Trie> trie;
    SwipeEngine engine;
};

// Copy up to `max` candidate words into `words_buf` as consecutive
// NUL-terminated UTF-8 strings, and their scores into `scores`. Returns the
// number written. Stops early if the buffer would overflow.
int emit(const std::vector<DecodedWord> &results, char *words_buf,
         int words_cap, float *scores, int max) {
    int count = 0;
    int off = 0;
    for (const auto &w : results) {
        if (count >= max) break;
        const int len = static_cast<int>(w.word.size());
        if (off + len + 1 > words_cap) break;
        std::memcpy(words_buf + off, w.word.data(), len);
        words_buf[off + len] = '\0';
        off += len + 1;
        scores[count] = w.final_score;
        ++count;
    }
    return count;
}

}  // namespace

extern "C" {

// Create an engine. `encoder_path` is required; `decoder_path`/`lm_path`/
// `lm_vocab_path` are optional ("" or NULL to omit). `dict_combined_path`
// is a FUTO `.combined` vocab file. Returns NULL on failure.
void *tilo_swipe_create(const char *encoder_path, const char *decoder_path,
                       const char *lm_path, const char *lm_vocab_path,
                       const char *dict_combined_path, int beam_width,
                       int top_k) {
    if (encoder_path == nullptr || encoder_path[0] == '\0') return nullptr;
    auto *h = new (std::nothrow) Handle();
    if (h == nullptr) return nullptr;

    if (dict_combined_path != nullptr && dict_combined_path[0] != '\0') {
        h->trie = swipe_decoder::load_trie_simple(dict_combined_path);
    }

    SwipeEngine::Config cfg;
    cfg.encoder_path = encoder_path;
    cfg.layout = swipe_decoder::default_en_qwerty();
    if (beam_width > 0) cfg.beam_width = beam_width;
    if (top_k > 0) cfg.top_k = top_k;
    if (h->trie) cfg.dictionaries.push_back(h->trie->itrie());
    if (decoder_path != nullptr && decoder_path[0] != '\0')
        cfg.decoder_path = decoder_path;
    if (lm_path != nullptr && lm_path[0] != '\0') cfg.lm_model_path = lm_path;
    if (lm_vocab_path != nullptr && lm_vocab_path[0] != '\0')
        cfg.lm_vocab_path = lm_vocab_path;

    if (!h->engine.init(cfg)) {
        delete h;
        return nullptr;
    }
    return h;
}

int tilo_swipe_has_decoder(void *handle) {
    auto *h = static_cast<Handle *>(handle);
    return (h != nullptr && h->engine.hasDecoder()) ? 1 : 0;
}

int tilo_swipe_has_lm(void *handle) {
    auto *h = static_cast<Handle *>(handle);
    return (h != nullptr && h->engine.hasLm()) ? 1 : 0;
}

// Recognize a swipe given normalized (x,y in [0,1]) points + times t (ms).
// Writes NUL-separated candidate words into words_buf and scores into
// scores[]. Returns the candidate count (<= max).
int tilo_swipe_recognize(void *handle, const float *x, const float *y,
                        const float *t, uintptr_t n, int top_k,
                        char *words_buf, int words_cap, float *scores,
                        int max) {
    auto *h = static_cast<Handle *>(handle);
    if (h == nullptr || x == nullptr || y == nullptr || t == nullptr || n == 0)
        return 0;
    std::vector<DecodedWord> results =
        h->engine.recognize(x, y, t, static_cast<size_t>(n), {}, top_k, 0, {});
    return emit(results, words_buf, words_cap, scores, max);
}

// Context-only next-word prediction (requires an LM). `context` is a
// NUL-separated list of `nctx` previous words.
int tilo_swipe_predict_next(void *handle, const char *context, int nctx, int k,
                           char *words_buf, int words_cap, float *scores,
                           int max) {
    auto *h = static_cast<Handle *>(handle);
    if (h == nullptr || !h->engine.hasLm()) return 0;
    std::vector<std::string> ctx;
    int off = 0;
    for (int i = 0; i < nctx && context != nullptr; ++i) {
        std::string w(context + off);
        off += static_cast<int>(w.size()) + 1;
        ctx.push_back(std::move(w));
    }
    auto preds = h->engine.predict_next(ctx, k);
    int count = 0;
    int woff = 0;
    for (const auto &p : preds) {
        if (count >= max) break;
        const int len = static_cast<int>(p.first.size());
        if (woff + len + 1 > words_cap) break;
        std::memcpy(words_buf + woff, p.first.data(), len);
        words_buf[woff + len] = '\0';
        woff += len + 1;
        scores[count] = p.second;
        ++count;
    }
    return count;
}

void tilo_swipe_destroy(void *handle) {
    delete static_cast<Handle *>(handle);
}

}  // extern "C"

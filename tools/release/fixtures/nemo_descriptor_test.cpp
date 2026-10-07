// SPDX-License-Identifier: Apache-2.0
// Review-only production loader regression. No ASR model or audio needed.
#include "runtime.h"

#include <fcntl.h>
#include <unistd.h>

#include <cstring>
#include <iostream>
#include <stdexcept>

static void require(bool result, const char* message) {
    if (!result) throw std::runtime_error(message);
}

template <typename F>
static void rejects(F operation, const char* message) {
    bool rejected = false;
    try {
        operation();
    } catch (const std::exception&) {
        rejected = true;
    }
    require(rejected, message);
}

int main(int argc, char** argv) {
    if (argc != 2) return 2;
    try {
        const std::string path = std::string(argv[1]) + "/fixture.gguf";
        const ggml_init_params params = {4096, nullptr, true};
        ggml_context* context = ggml_init(params);
        require(context != nullptr, "metadata allocation failed");
        ggml_tensor* tensor = ggml_new_tensor_1d(context, GGML_TYPE_F32, 4);
        ggml_set_name(tensor, "fixture");
        float values[] = {1, 2, 3, 4};
        tensor->data = values;
        gguf_context* metadata = gguf_init_empty();
        gguf_add_tensor(metadata, tensor);
        require(gguf_write_to_file(metadata, path.c_str(), false), "fixture write failed");
        gguf_free(metadata);
        ggml_free(context);

        const int writable = open(path.c_str(), O_RDWR | O_CLOEXEC);
        require(writable >= 0, "writable fixture open failed");
        rejects([&] { ggml_runtime::GGUFLoader loader("", writable); },
                "writable descriptor admitted");
        close(writable);
        rejects([&] { ggml_runtime::GGUFLoader loader("", -2); },
                "invalid descriptor fell back to path");
        const int input = open(path.c_str(), O_RDONLY | O_CLOEXEC);
        require(input >= 0, "fixture open failed");
        // Deliberately unusable path: the descriptor must supply metadata and tensors.
        ggml_runtime::GGUFLoader loader("/path/must/not/be/opened", input);
        close(input);
        require(unlink(path.c_str()) == 0, "fixture unlink failed");
        require(loader.has_tensor("fixture"), "descriptor metadata missing");
        require(std::memcmp(loader.get_tensor_file_data("fixture", sizeof(values)),
                            values, sizeof(values)) == 0, "descriptor tensor mismatch");
        rejects([&] { loader.get_tensor_file_data("fixture", SIZE_MAX); },
                "overflowing tensor extent admitted");
        loader.release_file_resources();
        require(std::memcmp(loader.get_tensor_file_data("fixture", sizeof(values)),
                            values, sizeof(values)) == 0, "lazy setup lost immutable anchor");
        loader.release_input();
        rejects([&] { loader.get_tensor_file_data("fixture", sizeof(values)); },
                "released descriptor reopened path");
        std::cout << "PASS owned descriptor metadata/tensors, caller close/unlink, "
                     "read-only requirement, invalid descriptor, overflow, released input\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}

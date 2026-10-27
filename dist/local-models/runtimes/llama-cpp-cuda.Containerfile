# llama.cpp's server for GGUF models on NVIDIA GPUs through CUDA, a local model
# runtime (tools/release/local-model-runtime.sh builds it;
# crates/agent/local-model-service/RELEASE.md, "Signed runtimes"). Built in
# NVIDIA's CUDA development image and run in its runtime image, both pinned by
# digest. CMake comes from Ubuntu's snapshot archive at a fixed time, so the
# build tools do not drift either; the runtime stage installs nothing.
ARG BUILD_BASE
ARG RUN_BASE
FROM ${BUILD_BASE} AS build
ARG UBUNTU_SNAPSHOT
ARG LLAMA_BUILD_NUMBER
ARG LLAMA_BUILD_COMMIT
COPY llama.cpp.tar.gz /llama.cpp.tar.gz
RUN set -eu; \
    rm -f /etc/apt/sources.list.d/*; \
    for suite in noble noble-updates noble-security; do \
      echo "deb [signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] http://snapshot.ubuntu.com/ubuntu/$UBUNTU_SNAPSHOT $suite main universe"; \
    done > /etc/apt/sources.list; \
    apt-get -o Acquire::Check-Valid-Until=false update; \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends cmake make g++; \
    mkdir /src && tar -xzf /llama.cpp.tar.gz -C /src --strip-components=1; \
    cmake -S /src -B /build-tree -DCMAKE_BUILD_TYPE=Release \
      -DLLAMA_BUILD_NUMBER="$LLAMA_BUILD_NUMBER" -DLLAMA_BUILD_COMMIT="$LLAMA_BUILD_COMMIT" \
      -DGGML_NATIVE=OFF -DGGML_BACKEND_DL=ON -DGGML_CPU_ALL_VARIANTS=ON -DGGML_CUDA=ON -DGGML_OPENMP=OFF \
      -DCMAKE_EXE_LINKER_FLAGS=-Wl,--allow-shlib-undefined \
      -DLLAMA_BUILD_SERVER=ON -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF \
      -DLLAMA_BUILD_UI=OFF -DLLAMA_USE_PREBUILT_UI=OFF -DLLAMA_OPENSSL=OFF -DLLAMA_CONNECT=OFF; \
    cmake --build /build-tree -j"$(nproc)"; \
    mkdir /app; \
    cp /build-tree/bin/llama-server /app/; \
    find /build-tree -name '*.so*' -exec cp -P {} /app/ \; ; \
    cp /src/LICENSE /app/LICENSE

FROM ${RUN_BASE}
COPY --from=build /app /app
# ggml loads its backends from the working directory; the driver's libraries
# arrive where the base image looks for them.
WORKDIR /app
ENV LD_LIBRARY_PATH=/app:/usr/local/nvidia/lib:/usr/local/nvidia/lib64
ENTRYPOINT ["/app/llama-server"]

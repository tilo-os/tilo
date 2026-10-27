# llama.cpp's server for GGUF models, the CPU and Vulkan local model runtimes
# (tools/release/local-model-runtime.sh builds it;
# crates/agent/local-model-service/RELEASE.md, "Signed runtimes").
#
# Every package comes from Fedora 44's release repository alone, installed into
# empty roots. That repository is frozen at the release, so with the base pinned
# by digest and the source pinned by hash the image is the same whenever it is
# built. Mesa's Vulkan drivers reach AMD, Intel and Arm Mali GPUs; the Vulkan
# build also carries every CPU backend, and runs on the processor where no GPU
# answers.
ARG BASE
FROM ${BASE} AS roots
ARG BACKEND
RUN set -eu; \
    build="bash coreutils findutils tar gzip gcc-c++ cmake make"; \
    run="glibc libstdc++ libgomp"; \
    case "$BACKEND" in \
      cpu) ;; \
      vulkan) build="$build glslc vulkan-headers vulkan-loader-devel spirv-headers-devel"; \
              run="$run vulkan-loader mesa-vulkan-drivers" ;; \
      *) echo "unknown backend $BACKEND" >&2; exit 1 ;; \
    esac; \
    for root in build run; do \
      eval "packages=\$$root"; \
      dnf5 --assumeyes --installroot="/$root" --use-host-config --releasever=44 \
        --repo=fedora --setopt=install_weak_deps=False --setopt=tsflags=nodocs \
        install $packages; \
      dnf5 --installroot="/$root" --use-host-config --releasever=44 clean all; \
      rm -rf "/$root/var/cache" "/$root/var/log" "/$root/var/lib/dnf"; \
    done

FROM scratch AS build
ARG BACKEND
ARG LLAMA_BUILD_NUMBER
ARG LLAMA_BUILD_COMMIT
COPY --from=roots /build /
COPY llama.cpp.tar.gz /llama.cpp.tar.gz
# No web UI and no HTTPS client: the service talks to the API on loopback, and
# the model is a file. Every CPU variant is built and the best one loaded at
# start, so one image serves every processor of its architecture.
RUN set -eu; \
    mkdir /src && tar -xzf /llama.cpp.tar.gz -C /src --strip-components=1; \
    vulkan=OFF; [ "$BACKEND" = vulkan ] && vulkan=ON; \
    cmake -S /src -B /build-tree -DCMAKE_BUILD_TYPE=Release \
      -DLLAMA_BUILD_NUMBER="$LLAMA_BUILD_NUMBER" -DLLAMA_BUILD_COMMIT="$LLAMA_BUILD_COMMIT" \
      -DGGML_NATIVE=OFF -DGGML_BACKEND_DL=ON -DGGML_CPU_ALL_VARIANTS=ON -DGGML_VULKAN="$vulkan" \
      -DLLAMA_BUILD_SERVER=ON -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF \
      -DLLAMA_BUILD_UI=OFF -DLLAMA_USE_PREBUILT_UI=OFF -DLLAMA_OPENSSL=OFF -DLLAMA_CONNECT=OFF; \
    cmake --build /build-tree -j"$(nproc)"; \
    mkdir /app; \
    cp /build-tree/bin/llama-server /app/; \
    find /build-tree -name '*.so*' -exec cp -P {} /app/ \; ; \
    cp /src/LICENSE /app/LICENSE

FROM scratch
COPY --from=roots /run /
COPY --from=build /app /app
# ggml loads its backends from the working directory.
WORKDIR /app
ENV LD_LIBRARY_PATH=/app
ENTRYPOINT ["/app/llama-server"]

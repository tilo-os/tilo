# ---- AirPlay sender ---------------------------------------------------------
# Doubletake's current protocol implementation is newer than its last binary
# release: it includes the CoreUtils/HAP pairing path required by current
# AirPlay receivers. Build one audited commit, run its hardware-free protocol
# suite, and copy only the static sender into the OS image. The patches make
# its cleanup tests wait for events instead of wall-clock deadlines.
FROM docker.io/library/golang:1.25.0-bookworm@sha256:81dc45d05a7444ead8c92a389621fafabc8e40f8fd1a19d7e5df14e61e98bc1a AS doubletake-builder
ENV GOPROXY=https://proxy.golang.org GOSUMDB=sum.golang.org
COPY os/external-assets/doubletake/ /patches/
RUN --mount=type=bind,from=external-assets,source=/,target=/tilo-assets,ro \
    mkdir -p /src /out \
    && tar -xzf /tilo-assets/doubletake-ae06722.tar.gz --strip-components=1 -C /src \
    && git -C /src apply /patches/*.patch
WORKDIR /src
RUN --mount=type=cache,target=/go/pkg/mod \
    --mount=type=cache,target=/root/.cache/go-build \
    CGO_ENABLED=0 go test ./... \
    && CGO_ENABLED=0 go build -trimpath -ldflags='-s -w -extldflags=-static' \
        -o /out/doubletake ./cmd/doubletake \
    && CGO_ENABLED=0 go build -trimpath -ldflags='-s -w -extldflags=-static' \
        -o /out/doubletake-test-receiver ./cmd/doubletake-test-receiver

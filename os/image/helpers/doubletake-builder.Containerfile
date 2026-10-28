# ---- AirPlay sender ---------------------------------------------------------
# Doubletake's current protocol implementation is newer than its last binary
# release: it includes the CoreUtils/HAP pairing path required by current
# AirPlay receivers. Build one audited commit, run its hardware-free protocol
# suite, and copy only the static sender into the OS image. The patches make
# its cleanup tests wait for events instead of wall-clock deadlines. Its Go
# modules are pinned assets too, laid out as a file GOPROXY at their proxy
# paths; go.sum verifies each, and the build has no network.
FROM docker.io/library/golang:1.25.0-bookworm@sha256:81dc45d05a7444ead8c92a389621fafabc8e40f8fd1a19d7e5df14e61e98bc1a AS doubletake-builder
ENV GOPROXY=file:///goproxy GOSUMDB=off GOTOOLCHAIN=local
COPY os/external-assets/doubletake/ /patches/
COPY os/external-assets/github-assets.tsv /tilo-assets.tsv
RUN --network=none --mount=type=bind,from=external-assets,source=/,target=/tilo-assets,ro \
    mkdir -p /src /out \
    && tar -xzf /tilo-assets/doubletake-ae06722.tar.gz --strip-components=1 -C /src \
    && git -C /src apply /patches/*.patch \
    && awk -F '\t' '$1 !~ /^#/ && sub("^https://proxy[.]golang[.]org/", "", $3) { print $2, $3 }' \
      /tilo-assets.tsv \
    | while read -r name path; do install -D -m 0644 "/tilo-assets/$name" "/goproxy/$path" || exit 1; done
WORKDIR /src
RUN --network=none --mount=type=cache,target=/root/.cache/go-build \
    CGO_ENABLED=0 go test ./... \
    && CGO_ENABLED=0 go build -trimpath -ldflags='-s -w -extldflags=-static' \
        -o /out/doubletake ./cmd/doubletake \
    && CGO_ENABLED=0 go build -trimpath -ldflags='-s -w -extldflags=-static' \
        -o /out/doubletake-test-receiver ./cmd/doubletake-test-receiver

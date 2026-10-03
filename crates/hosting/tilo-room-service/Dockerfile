# Built from the context scripts/build-image.sh assembles.
FROM debian:trixie-slim
RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates \
  && rm -rf /var/lib/apt/lists/* \
  && useradd --system --uid 10001 --home-dir /nonexistent --shell /usr/sbin/nologin tilo
COPY bin/tilo-relay bin/tilo-room-service /usr/local/bin/
COPY assets/ /srv/tilo-room-assets/
USER 10001
ENV TILO_ROOMS_ASSETS=/srv/tilo-room-assets \
    TILO_ROOMS_RELAY_BIN=/usr/local/bin/tilo-relay \
    TILO_ROOMS_STATE=/run/tilo-rooms
# 8790: room API and page assets (plain HTTP, behind the ingress).
# 9443: TLS front for <room>.<domain>, reached by SNI passthrough.
EXPOSE 8790 9443
ENTRYPOINT ["/usr/local/bin/tilo-room-service"]

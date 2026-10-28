# Restore the runtime resolver only after transient-state cleanup. A RUN with
# /etc/resolv.conf pointing into /run/NetworkManager makes BuildKit mount its
# build-time DNS file there, so /run cannot be emptied by bootc's lint gate.
FROM ${BASE_REF} AS runtime-resolver-link
RUN mkdir -p /out && ln -s ../run/NetworkManager/resolv.conf /out/resolv.conf

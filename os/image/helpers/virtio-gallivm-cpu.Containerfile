FROM builder-base AS virtio-gallivm-cpu
COPY os/scripts/virtio-gallivm-cpu.c /tmp/virtio-gallivm-cpu.c
RUN cc -std=c11 -Wall -Wextra -Werror -O2 -fPIC -shared \
    -Wl,-z,relro,-z,now /tmp/virtio-gallivm-cpu.c -o /virtio-gallivm-cpu.so

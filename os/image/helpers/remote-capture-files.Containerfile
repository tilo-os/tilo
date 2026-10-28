FROM scratch AS remote-capture-files
COPY os/etc/tilo/desktop.toml os/etc/tilo/cache-gc.toml /etc/tilo/
COPY os/etc/modprobe.d/tilo-remote-camera.conf /etc/modprobe.d/tilo-remote-camera.conf
COPY os/etc/modules-load.d/tilo-remote-camera.conf os/etc/modules-load.d/tilo-ddc.conf /etc/modules-load.d/

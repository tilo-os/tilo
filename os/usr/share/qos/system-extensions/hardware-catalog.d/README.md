This image-owned, separately signed catalogue maps observed PCI vendor/device
identities to exact BootPack catalogue ids. Every rule names both a vendor and
device; optional subsystem ids make a rule more specific. The privileged
manager selects only the most-specific matching rule for each observed device,
then rechecks the selected pack's independent signature and image/ABI pin.

The base image intentionally ships an empty release catalogue until a qOS
release publishes matching signed BootPack deployments. This is fail-closed:
an unknown device is never mapped to a guessed driver pack.

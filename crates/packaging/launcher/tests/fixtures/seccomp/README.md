# Native seccomp program references

These required x86_64 golden programs pin the production policy byte for byte.
Their LF hex representation contains one serialized eight-byte `sock_filter`
instruction per line, in the original little-endian byte order. The tests
independently decode these fixed bytes and compare them against the production
compiler; no policy is generated from the implementation under test.

The programs extend the original fixtures at `2a26be496c` only by the rules
refusing `io_uring_setup`, `io_uring_enter` and `io_uring_register` (x86_64
syscalls 425 to 427) with `EPERM`, and retain the repository's license. No network, asset cache or extra generation tool is
needed to compile or run these tests. Malformed hex fails the test.

Byte pins:

- `tilo-default.x86_64.bpf`: 2,240 bytes, SHA-256 `78f3b1535287612c4764f8fdf002702e7685005da48f6fb5a75f4a2b3ce549b3`.
- `tilo-jit.x86_64.bpf`: 2,048 bytes, SHA-256 `cc93621454ef900eba338a2e03e3026ede6279db973a1b3fe3ef8e967338e0ce`.
- `tilo-web-engine.x86_64.bpf`: 2,128 bytes, SHA-256 `b8e2c508a37db3a89beaddc378e262659664b758ccfac748af382004cad57f58`.

A deliberate policy change still requires review of the regenerated program
and its expected text bytes. Never replace the golden with a call to the
current compiler: that would erase the independent policy comparison.

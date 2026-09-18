/* Minimal relocatable ELF carrying the metadata kmod requires to inspect a
 * module signature. It is never linked or loaded into a kernel. */
const char tilo_test_license[] __attribute__((section(".modinfo"), used)) = "license=GPL";
const char tilo_test_name[] __attribute__((section(".modinfo"), used)) = "name=tilo_sig_test";

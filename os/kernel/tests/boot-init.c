#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/utsname.h>
#include <sys/reboot.h>
#include <unistd.h>
static int check_module(const char *path, int expected) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) { perror(path); return 0; }
    int result = syscall(SYS_finit_module, fd, "", 0);
    int error = result == 0 ? 0 : errno;
    close(fd);
    printf("TILO_MODULE_CHECK path=%s errno=%d expected=%d\n", path, error, expected);
    return error == expected;
}
int main(void) {
    struct utsname info;
    int valid = uname(&info) == 0;
    if (valid) printf("TILO_KERNEL_USERSPACE_OK release=%s machine=%s\n", info.release, info.machine);
    else puts("TILO_KERNEL_USERSPACE_FAILED");
    int signatures = check_module("/unsigned.ko", EKEYREJECTED);
    signatures &= check_module("/tampered.ko", EKEYREJECTED);
    signatures &= check_module("/signed.ko", 0);
    puts(signatures ? "TILO_MODULE_SIGNATURE_ENFORCEMENT_OK" : "TILO_MODULE_SIGNATURE_ENFORCEMENT_FAILED");
    fflush(stdout);
    sync();
    reboot(RB_POWER_OFF);
    for (;;) pause();
}

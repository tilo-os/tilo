#define _GNU_SOURCE

#include <errno.h>
#include <linux/reboot.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

static void print_firmware_byte(const char *label, const char *path)
{
    unsigned char encoded[5];
    FILE *file = fopen(path, "rb");
    if (file != NULL && fread(encoded, 1, sizeof(encoded), file) == sizeof(encoded))
        printf("TILO_SECURE_BOOT_%s=%u\n", label, encoded[4]);
    else
        printf("TILO_SECURE_BOOT_%s=missing\n", label);
    if (file != NULL)
        fclose(file);
}

static int run_write(const char *variable, const char *esl,
                     const char *key, const char *certificate)
{
    pid_t child = fork();
    if (child == 0) {
        if (key != NULL)
            execl("/tilo-crypto", "tilo-crypto", "firmware-write-esl",
                  "/sys/firmware/efi/efivars", variable, esl,
                  key, certificate, NULL);
        else
            execl("/tilo-crypto", "tilo-crypto", "firmware-write-esl",
                  "/sys/firmware/efi/efivars", variable, esl, NULL);
        _exit(127);
    }
    int status = 0;
    if (child < 0 || waitpid(child, &status, 0) != child)
        return -1;
    return WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : -1;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    puts("TILO_SECURE_BOOT_ENROLL_START");
    mkdir("/sys", 0755);
    if (mount("sysfs", "/sys", "sysfs", 0, NULL) != 0) {
        printf("TILO_SECURE_BOOT_ENROLL_FAIL sysfs errno=%d\n", errno);
        return 1;
    }
    mkdir("/sys/firmware/efi/efivars", 0755);
    if (mount("efivarfs", "/sys/firmware/efi/efivars", "efivarfs", 0, NULL) != 0) {
        printf("TILO_SECURE_BOOT_ENROLL_FAIL efivarfs errno=%d\n", errno);
        return 1;
    }
    print_firmware_byte("SETUP_MODE",
        "/sys/firmware/efi/efivars/SetupMode-8be4df61-93ca-11d2-aa0d-00e098032b8c");
    print_firmware_byte("SECURE_BOOT",
        "/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c");
    /* PK is deliberately last: writing it leaves SetupMode. */
    if (run_write("db", "/db.esl", NULL, NULL) != 0 ||
        run_write("KEK", "/KEK.esl", NULL, NULL) != 0 ||
        run_write("PK", "/PK.esl", "/PK.key", "/PK.crt") != 0) {
        puts("TILO_SECURE_BOOT_ENROLL_FAIL write");
        return 1;
    }
    puts("TILO_SECURE_BOOT_ENROLL_PASS");
    sync();
    reboot(LINUX_REBOOT_CMD_POWER_OFF);
    return 0;
}

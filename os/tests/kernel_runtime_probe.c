#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <linux/reboot.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/reboot.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

struct sched_attr {
    unsigned int size;
    unsigned int policy;
    unsigned long long flags;
    int nice;
    unsigned int priority;
    unsigned long long runtime;
    unsigned long long deadline;
    unsigned long long period;
    unsigned int util_min;
    unsigned int util_max;
};

#define IOPRIO_WHO_PROCESS 1
#define IOPRIO_CLASS_BEST_EFFORT 2
#define IOPRIO_CLASS_SHIFT 13
#define TILO_GAME_IOPRIO ((IOPRIO_CLASS_BEST_EFFORT << IOPRIO_CLASS_SHIFT) | 2)

static int probe_game_scheduling(void)
{
    struct sched_attr attr = {
        .size = sizeof(attr),
        .flags = 0x20, /* SCHED_FLAG_UTIL_CLAMP_MIN */
        .util_min = 256,
        .util_max = 1024,
    };
    if (syscall(SYS_sched_setattr, 0, &attr, 0) != 0)
        return -1;

    memset(&attr, 0, sizeof(attr));
    attr.size = sizeof(attr);
    if (syscall(SYS_sched_getattr, 0, &attr, sizeof(attr), 0) != 0 ||
        attr.util_min != 256) {
        errno = EPROTO;
        return -1;
    }
    if (syscall(SYS_prctl, 29 /* PR_SET_TIMERSLACK */, 1000UL, 0, 0, 0) != 0 ||
        syscall(SYS_prctl, 30 /* PR_GET_TIMERSLACK */, 0, 0, 0, 0) != 1000) {
        errno = EPROTO;
        return -1;
    }
    if (syscall(SYS_ioprio_set, IOPRIO_WHO_PROCESS, 0, TILO_GAME_IOPRIO) != 0 ||
        syscall(SYS_ioprio_get, IOPRIO_WHO_PROCESS, 0) != TILO_GAME_IOPRIO) {
        errno = EPROTO;
        return -1;
    }
    return 0;
}

static int load_module(const char *path)
{
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0)
        return -1;
    int rc = syscall(SYS_finit_module, fd, "", 0);
    int saved = errno;
    close(fd);
    errno = saved;
    return rc;
}

static void power_off(int success)
{
    sync();
    reboot(LINUX_REBOOT_CMD_POWER_OFF);
    _exit(success ? 0 : 1);
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    puts("TILO_RUNTIME_PROBE_START");

    if (probe_game_scheduling() != 0) {
        printf("TILO_RUNTIME_PROBE_ERROR scheduling errno=%d\n", errno);
        power_off(0);
    }
    puts("TILO_GAME_SCHEDULING uclamp_min=256 timer_slack_ns=1000 ioprio=be/2");

    int source = open("/signed.ko", O_RDONLY | O_CLOEXEC);
    struct stat st;
    if (source < 0 || fstat(source, &st) < 0 || st.st_size < 4096) {
        printf("TILO_RUNTIME_PROBE_ERROR source errno=%d\n", errno);
        power_off(0);
    }

    unsigned char *image = mmap(NULL, st.st_size, PROT_READ | PROT_WRITE,
                                MAP_PRIVATE, source, 0);
    if (image == MAP_FAILED) {
        printf("TILO_RUNTIME_PROBE_ERROR mmap errno=%d\n", errno);
        power_off(0);
    }

    /* Alter executable payload, leaving the appended signature intact. */
    image[4096] ^= 1;
    int altered = open("/tampered.ko", O_CREAT | O_TRUNC | O_WRONLY | O_CLOEXEC,
                       0600);
    if (altered < 0 || write(altered, image, st.st_size) != st.st_size) {
        printf("TILO_RUNTIME_PROBE_ERROR tamper errno=%d\n", errno);
        power_off(0);
    }
    close(altered);
    munmap(image, st.st_size);
    close(source);

    errno = 0;
    int rejected = load_module("/tampered.ko");
    int rejection_errno = errno;
    printf("TILO_TAMPERED_MODULE rc=%d errno=%d\n", rejected, rejection_errno);
    if (rejected == 0) {
        puts("TILO_RUNTIME_PROBE_FAIL tampered-module-loaded");
        power_off(0);
    }

    errno = 0;
    int accepted = load_module("/signed.ko");
    int acceptance_errno = errno;
    printf("TILO_SIGNED_MODULE rc=%d errno=%d\n", accepted, acceptance_errno);
    if (accepted != 0) {
        puts("TILO_RUNTIME_PROBE_FAIL signed-module-rejected");
        power_off(0);
    }

    puts("TILO_RUNTIME_PROBE_PASS");
    power_off(1);
}

/* Disposable VM init: signed modules, read-only 9p inputs, tmpfs overlay root. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static void fail(const char *operation) { perror(operation); exit(1); }
static void directory(const char *path) {
    char copy[512];
    if (snprintf(copy, sizeof(copy), "%s", path) >= (int)sizeof(copy)) fail("path length");
    for (char *cursor = copy + 1; ; ++cursor) {
        if (*cursor == '/' || !*cursor) {
            char saved = *cursor; *cursor = 0;
            if (mkdir(copy, 0755) && errno != EEXIST) fail(copy);
            *cursor = saved;
            if (!saved) break;
        }
    }
}
static void attach(const char *source, const char *target, const char *type,
                   unsigned long flags, const char *options) {
    directory(target);
    if (mount(source, target, type, flags, options)) fail(target);
}
static void module(const char *name) {
    char path[256];
    snprintf(path, sizeof(path), "/modules/%s.ko", name);
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0 || syscall(SYS_finit_module, fd, "", 0)) fail(path);
    close(fd);
}
static void prepare(void) {
    attach("proc", "/proc", "proc", 0, NULL);
    attach("sysfs", "/sys", "sysfs", 0, NULL);
    attach("devtmpfs", "/dev", "devtmpfs", 0, NULL);
    long abi = syscall(SYS_landlock_create_ruleset, NULL, 0, 1);
    printf("TILO_ARM_CAPSULE_VM_ABI=%ld\n", abi);
    if (abi < 5) { errno = ENOTSUP; fail("Landlock ABI"); }
    module("netfs"); module("9pnet"); module("9pnet_virtio"); module("9p"); module("overlay");
    int guest_block = access("/guest-squashfs", F_OK) == 0;
    if (guest_block) module("squashfs");
    attach("native", "/native", "9p", MS_RDONLY,
           "trans=virtio,version=9p2000.L,msize=1048576,cache=loose");
    attach("tmpfs", "/scratch", "tmpfs", MS_NOSUID | MS_NODEV, "mode=0755,size=2G");
    directory("/scratch/upper"); directory("/scratch/work");
    attach("overlay", "/vm", "overlay", 0,
           "lowerdir=/native,upperdir=/scratch/upper,workdir=/scratch/work");
    const char *tags[] = {"inputs", "x86", "proton", "steam"};
    const char *targets[] = {
        "/vm/inputs", "/vm/usr/lib/tilo/windows-runners/x86",
        "/vm/usr/lib/tilo/windows-runners/proton/UMU-Proton-10.0-4",
        "/vm/usr/lib/tilo/windows-runners/steam-runtime/SteamLinuxRuntime_sniper"
    };
    for (size_t i = 0; i < sizeof(tags)/sizeof(tags[0]); ++i) {
        if (guest_block && i == 1) {
            for (int attempt = 0; attempt < 100 && access("/dev/vda", F_OK); ++attempt)
                usleep(100000);
            attach("/dev/vda", targets[i], "squashfs", MS_RDONLY, NULL);
            puts("TILO_ARM_CAPSULE_GUEST_ROOT=squashfs-readonly");
        } else {
            attach(tags[i], targets[i], "9p", MS_RDONLY,
                   "trans=virtio,version=9p2000.L,msize=1048576,cache=loose");
        }
    }
    attach("proc", "/vm/proc", "proc", 0, NULL);
    attach("sysfs", "/vm/sys", "sysfs", 0, NULL);
    attach("devtmpfs", "/vm/dev", "devtmpfs", 0, NULL);
    attach("tmpfs", "/vm/tmp", "tmpfs", MS_NOSUID | MS_NODEV, "mode=1777");
    attach("tmpfs", "/vm/run", "tmpfs", MS_NOSUID | MS_NODEV, "mode=0755");
    attach("tmpfs", "/vm/dev/shm", "tmpfs", MS_NOSUID | MS_NODEV, "mode=1777");
    attach("devpts", "/vm/dev/pts", "devpts", MS_NOSUID | MS_NOEXEC, "newinstance,ptmxmode=0666,mode=0620");
}
int main(void) {
    if (getpid() != 1) return 2;
    setvbuf(stdout, NULL, _IONBF, 0);
    pid_t child = fork();
    if (child == 0) {
        prepare();
        /* Switch the namespace's root mount, not only the process root.
         * Linux intentionally refuses unprivileged userns creation in chroots. */
        if (chdir("/vm") || mount(".", "/", NULL, MS_MOVE, NULL) ||
            chroot(".") || chdir("/")) fail("switch root");
        execl("/usr/bin/bash", "bash", "/inputs/run.sh", (char *)NULL);
        fail("exec guest harness");
    }
    int status = -1;
    if (child < 0 || waitpid(child, &status, 0) != child) perror("wait init child");
    printf("TILO_ARM_CAPSULE_VM_STATUS=%d\n", status);
    puts(WIFEXITED(status) && WEXITSTATUS(status) == 0 ?
         "TILO_ARM_CAPSULE_VM_PASS" : "TILO_ARM_CAPSULE_VM_FAIL");
    sync(); reboot(RB_POWER_OFF);
    for (;;) pause();
}

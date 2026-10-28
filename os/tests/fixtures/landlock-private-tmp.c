#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>
static int probe(int allowed) {
    errno = 0;
    int fd = open("/tmp/probe-created", O_CREAT | O_EXCL | O_WRONLY, 0600);
    int error = errno;
    int valid = allowed ? fd >= 0 : fd == -1 && error == EACCES;
    if (fd >= 0) { close(fd); unlink("/tmp/probe-created"); }
    errno = 0;
    fd = open("/denied/no-write", O_CREAT | O_EXCL | O_WRONLY, 0600);
    int outside_error = errno;
    valid &= fd == -1 && outside_error == EACCES;
    if (fd >= 0) close(fd);
    printf("TILO_LANDLOCK_TMP_CHECK allowed=%d tmp_errno=%d outside_errno=%d valid=%d\n",
           allowed, error, outside_error, valid);
    return valid ? 0 : 1;
}
static int run(int allowed) {
    pid_t pid = fork();
    if (pid < 0) return 0;
    if (pid == 0) {
        if (allowed) execl("/usr/libexec/tilo/tilo-launcher", "tilo-launcher", "__sandbox-exec",
            "--ro", "/usr", "--ro", "/lib", "--ro", "/lib64", "--rw", "/tmp",
            "--net-restricted", "--", "/usr/bin/tmp-probe", "allow", (char *)NULL);
        else execl("/usr/libexec/tilo/tilo-launcher", "tilo-launcher", "__sandbox-exec",
            "--ro", "/usr", "--ro", "/lib", "--ro", "/lib64",
            "--net-restricted", "--", "/usr/bin/tmp-probe", "deny", (char *)NULL);
        perror("exec launcher");
        _exit(127);
    }
    int status;
    if (waitpid(pid, &status, 0) != pid) return 0;
    printf("TILO_LANDLOCK_CHILD allowed=%d status=%d\n", allowed, status);
    return WIFEXITED(status) && WEXITSTATUS(status) == 0;
}
int main(int argc, char **argv) {
    if (argc == 2) return probe(strcmp(argv[1], "allow") == 0);
    if (getpid() != 1) {
        fputs("The init fixture must run as PID 1 inside the test VM.\n", stderr);
        return 2;
    }
    setvbuf(stdout, NULL, _IONBF, 0);
    int valid = mount("proc", "/proc", "proc", 0, NULL) == 0;
    valid &= mount("devtmpfs", "/dev", "devtmpfs", 0, NULL) == 0;
    valid &= mount("tmpfs", "/tmp", "tmpfs", MS_NOSUID | MS_NODEV, "mode=1777") == 0;
    long abi = syscall(SYS_landlock_create_ruleset, NULL, 0, 1);
    printf("TILO_LANDLOCK_KERNEL_ABI=%ld\n", abi);
    valid &= abi >= 5;
    if (valid) {
        valid &= run(0);
        valid &= run(1);
    }
    puts(valid ? "TILO_LANDLOCK_PRIVATE_TMP_PASS" : "TILO_LANDLOCK_PRIVATE_TMP_FAIL");
    sync();
    reboot(RB_POWER_OFF);
    for (;;) pause();
}

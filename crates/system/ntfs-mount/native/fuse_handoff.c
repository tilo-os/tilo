/*
 * Preloaded into ntfs-3g when the NTFS mount service starts it confined
 * (crates/system/ntfs-mount/src/service/linux.rs).
 *
 * ntfs-3g opens the volume, then mounts its FUSE connection itself. A confined
 * process may not mount, and the service would not let a driver that has just
 * parsed a hostile volume choose mount options anyway. So this replaces
 * mount(2) for it:
 *
 *   1. it tells the service the volume opened: one byte, 'R', on HANDOFF_FD;
 *   2. the service mounts a FUSE connection of its own, with its own options,
 *      and answers 'G';
 *   3. it puts that connection (FUSE_FD) where ntfs-3g keeps its own (the
 *      "fd=N" mount option) and reports success.
 *
 * Any other mount, or any other answer, fails with EPERM. The service trusts
 * nothing from this side: all it ever reads here is the one byte.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define FUSE_FD 4
#define HANDOFF_FD 5

/* The descriptor named by "fd=N" in a FUSE mount's options, or -1. */
static int fuse_fd_option(const char *data)
{
    const char *option = data;
    while (option != NULL && *option != '\0') {
        if (strncmp(option, "fd=", 3) == 0) {
            char *end = NULL;
            long fd;
            errno = 0;
            fd = strtol(option + 3, &end, 10);
            if (errno != 0 || end == option + 3 || (*end != ',' && *end != '\0') || fd <= HANDOFF_FD
                || fd > 65535)
                return -1;
            return (int)fd;
        }
        option = strchr(option, ',');
        if (option != NULL)
            option++;
    }
    return -1;
}

static int handoff(int target)
{
    char byte = 'R';
    if (write(HANDOFF_FD, &byte, 1) != 1)
        return 0;
    if (read(HANDOFF_FD, &byte, 1) != 1 || byte != 'G')
        return 0;
    return dup2(FUSE_FD, target) == target;
}

int mount(const char *source, const char *target, const char *fstype, unsigned long flags,
          const void *data)
{
    int fd;
    int handed;
    (void)source;
    (void)target;
    (void)flags;
    if (fstype == NULL || data == NULL || strncmp(fstype, "fuse", 4) != 0) {
        errno = EPERM;
        return -1;
    }
    fd = fuse_fd_option(data);
    if (fd < 0) {
        errno = EPERM;
        return -1;
    }
    handed = handoff(fd);
    close(FUSE_FD);
    close(HANDOFF_FD);
    if (!handed) {
        errno = EPERM;
        return -1;
    }
    return 0;
}

/*
 * fdwatch — name the code that closes a descriptor it no longer owns.
 *
 * A descriptor is a number. Anything in a process that closes one it has
 * already closed, or that was never its own, frees that number for the
 * next accept(2) or open(2) — and from then on whoever *did* own it is
 * addressing a different file. The write still succeeds, the read still
 * blocks; nothing returns an error and nothing is logged. In the
 * compositor this has been showing up as a collaboration peer that ends
 * on one side and stays immortal on the other.
 *
 * Every close and dup2 in the shell's own Rust is ownership-based, so the
 * code that would do this is somewhere else: a C library the compositor
 * links, or a vendored dependency. Inference has already been tried and
 * it does not narrow far enough. This names the caller.
 *
 * Interposes close(2), dup2(2), dup3(2) and close_range(2) — the four
 * ways a descriptor number stops meaning what it meant — and for each
 * records:
 *
 *   - whether the descriptor being destroyed was open at all. A close of
 *     a descriptor that is already closed is the smoking gun: it is the
 *     only one of these that is unambiguously a bug.
 *   - what it named, when it was open: dev:ino, its kind, and for a
 *     socket its address family. The broker's peers are AF_UNIX sockets,
 *     so a close of one from a library with no business holding sockets
 *     is the second thing worth seeing.
 *   - who called: the return address, resolved through dladdr(3) to
 *     `<object>+0x<offset>` where it can be. `/proc/self/maps` is dumped
 *     beside the first record as well, so an address can still be
 *     resolved offline with addr2line if dladdr came back empty or was
 *     switched off.
 *
 * Switching it on
 * ---------------
 *   export LD_PRELOAD=/path/to/fdwatch.so
 *   export QOS_FDWATCH=1
 *   export QOS_FDWATCH_LOG=/somewhere/fdwatch-%p.log
 *   scripts/yc-demo-live.py …
 *
 * The rig builds every session's environment from `os.environ`
 * (`scripts/yc-demo.py`: `env = dict(os.environ)`), so exporting these in
 * the shell that starts a take reaches every compositor it spawns and,
 * through them, `qos-launcher` and every sandboxed app.
 * `scripts/fd-interpose/with-fdwatch.sh` does the exporting for one
 * command.
 *
 * Without QOS_FDWATCH set the library is inert — the wrappers call
 * straight through — so it is safe to leave in LD_PRELOAD between runs.
 *
 *   QOS_FDWATCH_LOG=<path>   where to write; `%p` becomes the pid, so
 *                            three sessions do not interleave into one
 *                            file. Default: stderr, which the rig already
 *                            captures into each session's compositor.log.
 *   QOS_FDWATCH_ALL=1        record every call, not only the two kinds
 *                            above. A firehose; useful once you know
 *                            which descriptor number you care about.
 *   QOS_FDWATCH_NORESOLVE=1  do not call dladdr(3) on the recording path.
 *                            It takes the dynamic linker's lock, which is
 *                            worth avoiding if a run deadlocks inside
 *                            dlopen with this armed. The raw address and
 *                            the maps dump are still recorded, so nothing
 *                            is lost but the convenience.
 *
 * Everything on the recording path is async-signal-safe or close to it:
 * no malloc, no stdio, a stack buffer and one write(2). A per-thread
 * guard keeps anything reached from here — dladdr in particular — from
 * recursing back in through its own close.
 */
#define _GNU_SOURCE

#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

static int (*real_close)(int);
static int (*real_dup2)(int, int);
static int (*real_dup3)(int, int, int);
static int (*real_close_range)(unsigned int, unsigned int, int);

static int armed;
static int log_fd = -1;
static int record_everything;
static int resolve_callers = 1;
static int maps_dumped;

/* Anything the recording path calls may itself close a descriptor.
   Without this, the first such call recurses forever. */
static __thread int inside;

static void resolve_reals(void)
{
    if (!real_close) {
        real_close = dlsym(RTLD_NEXT, "close");
    }
    if (!real_dup2) {
        real_dup2 = dlsym(RTLD_NEXT, "dup2");
    }
    if (!real_dup3) {
        real_dup3 = dlsym(RTLD_NEXT, "dup3");
    }
    if (!real_close_range) {
        real_close_range = dlsym(RTLD_NEXT, "close_range");
    }
}

__attribute__((constructor)) static void fdwatch_init(void)
{
    resolve_reals();

    const char *on = getenv("QOS_FDWATCH");
    if (!on || !*on || strcmp(on, "0") == 0) {
        return;
    }
    record_everything = getenv("QOS_FDWATCH_ALL") != NULL;
    resolve_callers = getenv("QOS_FDWATCH_NORESOLVE") == NULL;

    const char *path = getenv("QOS_FDWATCH_LOG");
    if (path && *path) {
        char expanded[4096];
        const char *pct = strstr(path, "%p");
        if (pct) {
            snprintf(expanded, sizeof expanded, "%.*s%d%s",
                     (int) (pct - path), path, (int) getpid(), pct + 2);
        } else {
            snprintf(expanded, sizeof expanded, "%s", path);
        }
        log_fd = open(expanded, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0644);
    }
    if (log_fd < 0) {
        log_fd = 2;
    }
    armed = 1;
}

static void emit(const char *bytes, size_t len)
{
    ssize_t written = write(log_fd, bytes, len);
    (void) written;
}

static void emit_str(const char *s)
{
    emit(s, strlen(s));
}

/* The address space, once, beside the first record — so a return address
   can be turned into a file and a line offline even when dladdr could
   not, or was switched off. */
static void dump_maps_once(void)
{
    if (maps_dumped) {
        return;
    }
    maps_dumped = 1;
    int maps = open("/proc/self/maps", O_RDONLY | O_CLOEXEC);
    if (maps < 0) {
        return;
    }
    emit_str("fdwatch --- begin /proc/self/maps ---\n");
    char buf[8192];
    ssize_t n;
    while ((n = read(maps, buf, sizeof buf)) > 0) {
        emit(buf, (size_t) n);
    }
    emit_str("fdwatch --- end /proc/self/maps ---\n");
    if (real_close) {
        real_close(maps);
    }
}

/* Describe the descriptor that is about to stop meaning what it means.
   `*was_open` is the one that matters most: a close of something already
   closed is a close of a number somebody else may since have been
   given. */
static void describe(int fd, char *out, size_t cap, int *was_open, int *is_socket)
{
    *was_open = 0;
    *is_socket = 0;

    if (fcntl(fd, F_GETFD) == -1) {
        snprintf(out, cap, "was-not-open");
        return;
    }
    *was_open = 1;

    struct stat st;
    if (fstat(fd, &st) != 0) {
        snprintf(out, cap, "open but unstattable");
        return;
    }

    char kind = '?';
    if (S_ISSOCK(st.st_mode)) {
        kind = 's';
        *is_socket = 1;
    } else if (S_ISREG(st.st_mode)) {
        kind = 'f';
    } else if (S_ISFIFO(st.st_mode)) {
        kind = 'p';
    } else if (S_ISCHR(st.st_mode)) {
        kind = 'c';
    } else if (S_ISDIR(st.st_mode)) {
        kind = 'd';
    }

    if (*is_socket) {
        int domain = -1;
        socklen_t len = sizeof domain;
        if (getsockopt(fd, SOL_SOCKET, SO_DOMAIN, &domain, &len) != 0) {
            domain = -1;
        }
        snprintf(out, cap, "socket domain=%d dev=%lu ino=%lu", domain,
                 (unsigned long) st.st_dev, (unsigned long) st.st_ino);
    } else {
        snprintf(out, cap, "kind=%c dev=%lu ino=%lu", kind,
                 (unsigned long) st.st_dev, (unsigned long) st.st_ino);
    }
}

static void name_caller(void *addr, char *out, size_t cap)
{
    if (resolve_callers) {
        Dl_info info;
        if (dladdr(addr, &info) && info.dli_fname && info.dli_fbase) {
            snprintf(out, cap, "%s+0x%lx [%p]", info.dli_fname,
                     (unsigned long) ((const char *) addr - (const char *) info.dli_fbase),
                     addr);
            return;
        }
    }
    snprintf(out, cap, "[%p]", addr);
}

/* `doomed` is the descriptor that stops being what it was: the argument
   for close, the *target* for dup2 and dup3. `source` is -1 unless one
   descriptor is being put on top of another. */
static void record(const char *op, int doomed, int source, void *caller)
{
    if (!armed || inside) {
        return;
    }
    if (doomed == log_fd || source == log_fd) {
        return;
    }
    inside = 1;

    char what[192];
    int was_open = 0;
    int is_socket = 0;
    describe(doomed, what, sizeof what, &was_open, &is_socket);

    /* The default is not a firehose: a descriptor that was already
       closed, or one that names a socket. Everything else is ordinary
       housekeeping and there is a great deal of it. */
    if (record_everything || !was_open || is_socket) {
        dump_maps_once();

        char who[512];
        name_caller(caller, who, sizeof who);

        char line[1024];
        int n;
        if (source >= 0) {
            n = snprintf(line, sizeof line,
                         "fdwatch pid=%d tid=%ld %s source=%d target=%d %s from %s\n",
                         (int) getpid(), (long) syscall(SYS_gettid), op, source,
                         doomed, what, who);
        } else {
            n = snprintf(line, sizeof line,
                         "fdwatch pid=%d tid=%ld %s fd=%d %s from %s\n",
                         (int) getpid(), (long) syscall(SYS_gettid), op, doomed,
                         what, who);
        }
        if (n > 0) {
            size_t len = (size_t) n < sizeof line ? (size_t) n : sizeof line - 1;
            emit(line, len);
        }
    }

    inside = 0;
}

int close(int fd)
{
    if (!real_close) {
        resolve_reals();
    }
    record("close", fd, -1, __builtin_return_address(0));
    if (real_close) {
        return real_close(fd);
    }
    return (int) syscall(SYS_close, fd);
}

int dup2(int oldfd, int newfd)
{
    if (!real_dup2) {
        resolve_reals();
    }
    record("dup2", newfd, oldfd, __builtin_return_address(0));
    if (real_dup2) {
        return real_dup2(oldfd, newfd);
    }
    return (int) syscall(SYS_dup2, oldfd, newfd);
}

int dup3(int oldfd, int newfd, int flags)
{
    if (!real_dup3) {
        resolve_reals();
    }
    record("dup3", newfd, oldfd, __builtin_return_address(0));
    if (real_dup3) {
        return real_dup3(oldfd, newfd, flags);
    }
    return (int) syscall(SYS_dup3, oldfd, newfd, flags);
}

int close_range(unsigned int first, unsigned int last, int flags)
{
    if (!real_close_range) {
        resolve_reals();
    }
    if (armed && !inside) {
        /* Reported as its endpoints rather than one line per descriptor:
           the range is usually most of the table, and the interesting
           part is that something swept it at all. */
        void *caller = __builtin_return_address(0);
        inside = 1;
        dump_maps_once();
        char who[512];
        name_caller(caller, who, sizeof who);
        char line[1024];
        int n = snprintf(line, sizeof line,
                         "fdwatch pid=%d tid=%ld close_range first=%u last=%u flags=%d from %s\n",
                         (int) getpid(), (long) syscall(SYS_gettid), first, last,
                         flags, who);
        if (n > 0) {
            size_t len = (size_t) n < sizeof line ? (size_t) n : sizeof line - 1;
            emit(line, len);
        }
        inside = 0;
    }
    if (real_close_range) {
        return real_close_range(first, last, flags);
    }
    return (int) syscall(SYS_close_range, first, last, flags);
}

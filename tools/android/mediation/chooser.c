// SPDX-License-Identifier: MIT
// No filesystem or D-Bus fallback. Only an inherited authenticated connection.
#define _GNU_SOURCE
#include <jni.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdlib.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#define FRAME_LIMIT 65536
static int connection = -2;

static long milliseconds(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now)) return -1;
    return now.tv_sec * 1000 + now.tv_nsec / 1000000;
}

static int ready(int fd, short event, long deadline) {
    for (;;) {
        long now = milliseconds();
        long remaining = deadline - now;
        if (now < 0 || remaining <= 0 || remaining > 120000) return 0;
        struct pollfd descriptor = {.fd = fd, .events = event};
        int result = poll(&descriptor, 1, (int)remaining);
        if (result < 0 && errno == EINTR) continue;
        return result > 0 && (descriptor.revents & event);
    }
}

static int connect_inherited(void) {
    const char *text = getenv("TILO_SVC_CHOOSER_FD");
    char *end = NULL;
    if (!text || !*text) return -1;
    errno = 0;
    long fd = strtol(text, &end, 10);
    if (errno || *end || fd < 3 || fd > 1048576) return -1;
    struct sockaddr_storage peer;
    socklen_t length = sizeof(peer);
    int kind = 0;
    socklen_t size = sizeof(kind);
    if (getpeername((int)fd, (struct sockaddr *)&peer, &length) || peer.ss_family != AF_UNIX ||
        getsockopt((int)fd, SOL_SOCKET, SO_TYPE, &kind, &size) || kind != SOCK_STREAM)
        return -1;
    int flags = fcntl((int)fd, F_GETFD);
    if (flags < 0 || fcntl((int)fd, F_SETFD, flags | FD_CLOEXEC)) return -1;
    return fcntl((int)fd, F_DUPFD_CLOEXEC, 3);
}

// Java serializes all requests. A timeout permanently ends this stream: a
// delayed reply cannot be mistaken for the response to a subsequent request.
JNIEXPORT jbyteArray JNICALL Java_android_app_TiloChooser_exchange(JNIEnv *env, jclass klass, jbyteArray request) {
    (void)klass;
    if (connection == -2) connection = connect_inherited();
    if (connection < 0 || !request) return NULL;
    jsize size = (*env)->GetArrayLength(env, request);
    if (size < 1 || size > FRAME_LIMIT) return NULL;
    jbyte *bytes = (*env)->GetByteArrayElements(env, request, NULL);
    if (!bytes) return NULL;
    long deadline = milliseconds() + 120000;
    int sent = 0;
    while (sent < size && ready(connection, POLLOUT, deadline)) {
        ssize_t amount = send(connection, bytes + sent, (size_t)(size - sent), MSG_NOSIGNAL | MSG_DONTWAIT);
        if (amount < 0 && (errno == EINTR || errno == EAGAIN)) continue;
        if (amount <= 0) break;
        sent += (int)amount;
    }
    (*env)->ReleaseByteArrayElements(env, request, bytes, JNI_ABORT);
    char reply[FRAME_LIMIT];
    int used = 0;
    if (sent == size) {
        while (used < FRAME_LIMIT && ready(connection, POLLIN, deadline)) {
            ssize_t amount = recv(connection, reply + used, 1, MSG_DONTWAIT);
            if (amount < 0 && (errno == EINTR || errno == EAGAIN)) continue;
            if (amount != 1) break;
            if (reply[used++] == '\n') {
                jbyteArray result = (*env)->NewByteArray(env, used);
                if (result) (*env)->SetByteArrayRegion(env, result, 0, used, (const jbyte *)reply);
                return result;
            }
        }
    }
    shutdown(connection, SHUT_RDWR);
    close(connection);
    connection = -1;
    return NULL;
}

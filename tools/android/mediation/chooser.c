// SPDX-License-Identifier: MIT
// Document selection uses only the inherited authenticated chooser connection.
// Metadata opens existing files inside the unchanged launcher sandbox.
#define _GNU_SOURCE
#include <jni.h>
#include <gio/gio.h>
#include <gtk/gtk.h>
#include <stdint.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdlib.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#define FRAME_LIMIT 65536
static int connection = -2;

// The hash-pinned NotificationManager owns this live GNotification builder.
// Category conveys persistence without elevating urgency or granting services.
JNIEXPORT void JNICALL Java_android_app_TiloChooser_markOngoingNative(
        JNIEnv *env, jclass klass, jlong builder) {
    (void)env;
    (void)klass;
    if (builder) g_notification_set_category((GNotification *)(intptr_t)builder,
            "x-tilo.android-ongoing");
}

static gchar *java_utf8(JNIEnv *env, jstring value, jsize limit) {
    if (!value) return NULL;
    jsize length = (*env)->GetStringLength(env, value);
    if (length > limit) return NULL;
    const jchar *characters = (*env)->GetStringChars(env, value, NULL);
    if (!characters) return NULL;
    gchar *text = NULL;
    // Embedded NUL cannot name a file or metadata column faithfully.
    gboolean valid = TRUE;
    for (jsize i = 0; i < length; i++) if (!characters[i]) valid = FALSE;
    if (valid) text = g_utf16_to_utf8((const gunichar2 *)characters, length, NULL, NULL, NULL);
    (*env)->ReleaseStringChars(env, value, characters);
    return text;
}

// The entrypoint owns this launch-local anonymous descriptor. It conveys no
// filesystem, notification, network or service privilege to a recovered child.
JNIEXPORT void JNICALL Java_android_app_TiloChooser_recoveryJournalNative(
        JNIEnv *env, jclass klass, jstring services) {
    (void)klass;
    const char *value = getenv("TILO_ANDROID_RECOVERY_FD");
    char *end = NULL;
    if (!value || !*value) return;
    errno = 0;
    long descriptor = strtol(value, &end, 10);
    if (errno || !end || *end || descriptor < 3 || descriptor > INT32_MAX) return;
    gchar *text = java_utf8(env, services, 4095);
    if (!text) return;
    size_t size = strlen(text);
    if (ftruncate((int)descriptor, 0) ||
            pwrite((int)descriptor, text, size, 0) != (ssize_t)size) {
        ftruncate((int)descriptor, 0);
        jclass error = (*env)->FindClass(env, "java/lang/IllegalStateException");
        if (error) (*env)->ThrowNew(env, error, "foreground recovery journal unavailable");
    }
    g_free(text);
}

static jstring utf8_java(JNIEnv *env, const char *value) {
    if (!value) return NULL;
    glong length;
    gunichar2 *characters = g_utf8_to_utf16(value, -1, NULL, &length, NULL);
    if (!characters) return NULL;
    jstring result = (*env)->NewString(env, (const jchar *)characters, (jsize)length);
    g_free(characters);
    return result;
}

// Metadata requires a successful read-authorized open in the existing sandbox.
// fstat does not read content. Never reopen a path after this authority
// check: a swapped symlink must not disclose another file's metadata.
JNIEXPORT jobjectArray JNICALL Java_android_app_TiloDocuments_queryFileNative(
        JNIEnv *env, jclass klass, jstring path, jobjectArray columns) {
    (void)klass;
    if (!columns || (*env)->GetArrayLength(env, columns) > 32) return NULL;
    gchar *filename = java_utf8(env, path, 4096);
    if (!filename || filename[0] != '/') { g_free(filename); return NULL; }
    int fd = open(filename, O_RDONLY | O_CLOEXEC | O_NONBLOCK | O_NOCTTY);
    if (fd < 0) { g_free(filename); return NULL; }
    struct stat metadata;
    int observed = fstat(fd, &metadata);
    close(fd);
    if (observed || !S_ISREG(metadata.st_mode)) { g_free(filename); return NULL; }
    jclass object_class = (*env)->FindClass(env, "java/lang/Object");
    jclass long_class = (*env)->FindClass(env, "java/lang/Long");
    if (!object_class || !long_class) { g_free(filename); return NULL; }
    jmethodID value_of = (*env)->GetStaticMethodID(env, long_class, "valueOf", "(J)Ljava/lang/Long;");
    if (!value_of) { g_free(filename); return NULL; }
    jsize count = (*env)->GetArrayLength(env, columns);
    jobjectArray row = (*env)->NewObjectArray(env, count, object_class, NULL);
    if (!row) { g_free(filename); return NULL; }
    for (jsize i = 0; i < count && !(*env)->ExceptionCheck(env); i++) {
        jstring column = (*env)->GetObjectArrayElement(env, columns, i);
        gchar *name = java_utf8(env, column, 128);
        (*env)->DeleteLocalRef(env, column);
        if (!name) { g_free(filename); return NULL; }
        jobject value = NULL;
        if (g_str_equal(name, "_display_name")) {
            gchar *basename = g_path_get_basename(filename);
            value = utf8_java(env, basename);
            g_free(basename);
        } else if (g_str_equal(name, "_size")) {
            value = (*env)->CallStaticObjectMethod(env, long_class, value_of, (jlong)metadata.st_size);
        } else if (g_str_equal(name, "mime_type")) {
            gchar *type = g_content_type_guess(filename, NULL, 0, NULL);
            gchar *mime = type ? g_content_type_get_mime_type(type) : NULL;
            value = utf8_java(env, mime);
            g_free(mime);
            g_free(type);
        }
        g_free(name);
        if (value && !(*env)->ExceptionCheck(env)) (*env)->SetObjectArrayElement(env, row, i, value);
        if (value) (*env)->DeleteLocalRef(env, value);
    }
    g_free(filename);
    return (*env)->ExceptionCheck(env) ? NULL : row;
}

// ATL wraps its GtkLabel inside a generic WrapperWidget and GtkBox. Generic
// roles do not expose an accessible name. Label the TextView's real semantic
// widget, using only public GTK child traversal and a bounded depth.
static GtkWidget *text_label(GtkWidget *widget, int depth) {
    if (GTK_IS_LABEL(widget)) return widget;
    if (!depth) return NULL;
    int children = 0;
    for (GtkWidget *child = gtk_widget_get_first_child(widget); child && children++ < 32;
         child = gtk_widget_get_next_sibling(child)) {
        GtkWidget *label = text_label(child, depth - 1);
        if (label) return label;
    }
    return NULL;
}

JNIEXPORT void JNICALL Java_android_app_TiloAccessibility_describeNative(JNIEnv *env, jclass klass,
        jlong widget, jstring label, jboolean text_view) {
    (void)klass;
    GtkAccessible *accessible = (GtkAccessible *)(uintptr_t)widget;
    if (!accessible || !GTK_IS_ACCESSIBLE(accessible)) return;
    if (text_view) {
        GtkWidget *target = text_label(GTK_WIDGET(accessible), 3);
        if (!target) return;
        accessible = GTK_ACCESSIBLE(target);
    }
    if (!label) {
        gtk_accessible_reset_property(accessible, GTK_ACCESSIBLE_PROPERTY_LABEL);
        return;
    }
    jsize length = (*env)->GetStringLength(env, label);
    if (length > 4096) return;
    const jchar *characters = (*env)->GetStringChars(env, label, NULL);
    if (!characters) return;
    gchar *text = g_utf16_to_utf8((const gunichar2 *)characters, length, NULL, NULL, NULL);
    (*env)->ReleaseStringChars(env, label, characters);
    if (!text) return;
    gtk_accessible_update_property(accessible, GTK_ACCESSIBLE_PROPERTY_LABEL, text, -1);
    g_free(text);
}

// Reuse ATL's already registered GApplication connection. The launcher exposes
// this bus only through its filtered proxy; without a declared via-host portal
// the notification query is denied. Never open another bus or activate a daemon.
JNIEXPORT jboolean JNICALL Java_android_app_TiloChooser_notificationServiceAvailable(JNIEnv *env, jclass klass) {
    (void)env;
    (void)klass;
    GApplication *application = g_application_get_default();
    if (!application || !g_application_get_is_registered(application)) return JNI_FALSE;
    GDBusConnection *bus = g_application_get_dbus_connection(application);
    if (!bus) return JNI_FALSE;
    GVariant *reply = g_dbus_connection_call_sync(bus, "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications", "org.freedesktop.Notifications",
        "GetServerInformation", NULL, G_VARIANT_TYPE("(ssss)"),
        G_DBUS_CALL_FLAGS_NO_AUTO_START, 2000, NULL, NULL);
    if (!reply) return JNI_FALSE;
    const char *name, *vendor, *version, *spec;
    g_variant_get(reply, "(&s&s&s&s)", &name, &vendor, &version, &spec);
    gboolean qualified = g_str_equal(name, "Tilo Notifications") && g_str_equal(vendor, "Tilo");
    g_variant_unref(reply);
    return qualified ? JNI_TRUE : JNI_FALSE;
}

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

/* SPDX-License-Identifier: AGPL-3.0-only
 * Private GTK 4 Wayland transaction-wrapper prototype; no installed activation.
 * Forward GtkIMContext semantics and field privacy metadata unchanged. Only
 * GtkTextView's native buffer grouping is extended around one commit signal.
 */
#include <gtk/gtk.h>
#include <gio/gio.h>
#include <gmodule.h>

typedef struct {
  GtkIMContext parent;
  GtkIMContext *delegate;
  GWeakRef widget;
} TiloAtomicIM;
typedef struct { GtkIMContextClass parent; } TiloAtomicIMClass;
G_DEFINE_DYNAMIC_TYPE(TiloAtomicIM, tilo_atomic_im, GTK_TYPE_IM_CONTEXT)

static TiloAtomicIM *atomic(GtkIMContext *context) {
  return (TiloAtomicIM *)context;
}

static void forward_commit(GtkIMContext *delegate, const char *text, gpointer data) {
  TiloAtomicIM *self = g_object_ref(data);
  GtkWidget *widget = g_weak_ref_get(&self->widget);
  GtkTextBuffer *buffer = NULL;
  (void)delegate;
  if (widget && GTK_IS_TEXT_VIEW(widget)) {
    buffer = g_object_ref(gtk_text_view_get_buffer(GTK_TEXT_VIEW(widget)));
    gtk_text_buffer_begin_user_action(buffer);
  }
  g_signal_emit_by_name(self, "commit", text);
  if (buffer) {
    gtk_text_buffer_end_user_action(buffer);
    g_object_unref(buffer);
    g_printerr("TILO_GTK_ATOMIC_COMMIT native text-view transaction completed\n");
  }
  if (widget) g_object_unref(widget);
  g_object_unref(self);
}

static void preedit_start(GtkIMContext *delegate, gpointer data) {
  (void)delegate; g_signal_emit_by_name(data, "preedit-start");
}
static void preedit_changed(GtkIMContext *delegate, gpointer data) {
  (void)delegate; g_signal_emit_by_name(data, "preedit-changed");
}
static void preedit_end(GtkIMContext *delegate, gpointer data) {
  (void)delegate; g_signal_emit_by_name(data, "preedit-end");
}
static gboolean retrieve_surrounding(GtkIMContext *delegate, gpointer data) {
  gboolean handled = FALSE;
  (void)delegate;
  g_signal_emit_by_name(data, "retrieve-surrounding", &handled);
  return handled;
}
static gboolean delete_surrounding(GtkIMContext *delegate, int offset, int count, gpointer data) {
  gboolean handled = FALSE;
  (void)delegate;
  g_signal_emit_by_name(data, "delete-surrounding", offset, count, &handled);
  return handled;
}
static gboolean invalid_composition(GtkIMContext *delegate, const char *text, gpointer data) {
  gboolean handled = FALSE;
  (void)delegate;
  g_signal_emit_by_name(data, "invalid-composition", text, &handled);
  return handled;
}

static void set_client_widget(GtkIMContext *context, GtkWidget *widget) {
  TiloAtomicIM *self = atomic(context);
  g_weak_ref_set(&self->widget, widget);
  gtk_im_context_set_client_widget(self->delegate, widget);
}
static void get_preedit_string(GtkIMContext *context, char **text, PangoAttrList **attrs, int *cursor) {
  gtk_im_context_get_preedit_string(atomic(context)->delegate, text, attrs, cursor);
}
static gboolean filter_keypress(GtkIMContext *context, GdkEvent *event) {
  return gtk_im_context_filter_keypress(atomic(context)->delegate, event);
}
static void focus_in(GtkIMContext *context) { gtk_im_context_focus_in(atomic(context)->delegate); }
static void focus_out(GtkIMContext *context) { gtk_im_context_focus_out(atomic(context)->delegate); }
static void reset(GtkIMContext *context) { gtk_im_context_reset(atomic(context)->delegate); }
static gboolean activate_osk(GtkIMContext *context, GdkEvent *event) {
  return gtk_im_context_activate_osk(atomic(context)->delegate, event);
}
static void set_cursor_location(GtkIMContext *context, GdkRectangle *rect) {
  gtk_im_context_set_cursor_location(atomic(context)->delegate, rect);
}
static void set_use_preedit(GtkIMContext *context, gboolean enabled) {
  gtk_im_context_set_use_preedit(atomic(context)->delegate, enabled);
}
static void set_surrounding(GtkIMContext *context, const char *text, int len, int cursor, int anchor) {
  gtk_im_context_set_surrounding_with_selection(atomic(context)->delegate, text, len, cursor, anchor);
}
static gboolean get_surrounding(GtkIMContext *context, char **text, int *cursor, int *anchor) {
  return gtk_im_context_get_surrounding_with_selection(atomic(context)->delegate, text, cursor, anchor);
}

static void dispose(GObject *object) {
  TiloAtomicIM *self = (TiloAtomicIM *)object;
  if (self->delegate) {
    g_signal_handlers_disconnect_by_data(self->delegate, self);
    gtk_im_context_set_client_widget(self->delegate, NULL);
    g_clear_object(&self->delegate);
  }
  G_OBJECT_CLASS(tilo_atomic_im_parent_class)->dispose(object);
}
static void finalize(GObject *object) {
  g_weak_ref_clear(&((TiloAtomicIM *)object)->widget);
  G_OBJECT_CLASS(tilo_atomic_im_parent_class)->finalize(object);
}
static void tilo_atomic_im_class_init(TiloAtomicIMClass *klass) {
  GtkIMContextClass *im = GTK_IM_CONTEXT_CLASS(klass);
  GObjectClass *object = G_OBJECT_CLASS(klass);
  object->dispose = dispose; object->finalize = finalize;
  im->set_client_widget = set_client_widget;
  im->get_preedit_string = get_preedit_string;
  im->filter_keypress = filter_keypress;
  im->focus_in = focus_in; im->focus_out = focus_out; im->reset = reset;
  im->set_cursor_location = set_cursor_location;
  im->set_use_preedit = set_use_preedit;
  im->set_surrounding_with_selection = set_surrounding;
  im->get_surrounding_with_selection = get_surrounding;
  /* This slot is private in GTK's class API: the prototype is pinned to the
   * exact 4.22.5 runtime and is not a portable or activated system module. */
  im->activate_osk_with_event = activate_osk;
}
static void tilo_atomic_im_class_finalize(TiloAtomicIMClass *klass) { (void)klass; }
static void tilo_atomic_im_init(TiloAtomicIM *self) {
  GIOExtensionPoint *point = g_io_extension_point_lookup("gtk-im-module");
  GIOExtension *extension = point ? g_io_extension_point_get_extension_by_name(point, "wayland") : NULL;
  if (!extension) g_error("Tilo atomic prototype requires the existing GTK Wayland context");
  g_weak_ref_init(&self->widget, NULL);
  self->delegate = g_object_new(g_io_extension_get_type(extension), NULL);
  /* The delegate must see the same purpose/hints, including passwords and
   * sensitive content. This prototype never normalizes them to plain text. */
  g_object_bind_property(self, "input-purpose", self->delegate, "input-purpose", G_BINDING_SYNC_CREATE);
  g_object_bind_property(self, "input-hints", self->delegate, "input-hints", G_BINDING_SYNC_CREATE);
  g_signal_connect(self->delegate, "commit", G_CALLBACK(forward_commit), self);
  g_signal_connect(self->delegate, "preedit-start", G_CALLBACK(preedit_start), self);
  g_signal_connect(self->delegate, "preedit-changed", G_CALLBACK(preedit_changed), self);
  g_signal_connect(self->delegate, "preedit-end", G_CALLBACK(preedit_end), self);
  g_signal_connect(self->delegate, "retrieve-surrounding", G_CALLBACK(retrieve_surrounding), self);
  g_signal_connect(self->delegate, "delete-surrounding", G_CALLBACK(delete_surrounding), self);
  g_signal_connect(self->delegate, "invalid-composition", G_CALLBACK(invalid_composition), self);
}
G_MODULE_EXPORT void g_io_module_load(GIOModule *module) {
  tilo_atomic_im_register_type(G_TYPE_MODULE(module));
  g_io_extension_point_implement("gtk-im-module", tilo_atomic_im_get_type(), "tilo-atomic", 0);
}
G_MODULE_EXPORT void g_io_module_unload(GIOModule *module) { (void)module; }
G_MODULE_EXPORT char **g_io_module_query(void) {
  char **points = g_new0(char *, 2);
  points[0] = g_strdup("gtk-im-module");
  return points;
}

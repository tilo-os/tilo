/* SPDX-License-Identifier: AGPL-3.0-only
 * Private qualification fixture using standard GTK 3 / GtkSourceView controls.
 * No editing, input-method, or undo override.
 */
#include <gtk/gtk.h>
#include <gtksourceview/gtksource.h>
static const char *mode;
static gchar *text;
static gboolean focused(GtkWidget *widget, GdkEventFocus *event, gpointer data) {
  (void)event; (void)data;
  guint purpose = GTK_IS_ENTRY(widget) ? gtk_entry_get_input_purpose(GTK_ENTRY(widget))
                                      : gtk_text_view_get_input_purpose(GTK_TEXT_VIEW(widget));
  guint hints = GTK_IS_ENTRY(widget) ? gtk_entry_get_input_hints(GTK_ENTRY(widget))
                                    : gtk_text_view_get_input_hints(GTK_TEXT_VIEW(widget));
  g_print("GTK3_NATIVE_FIELD_FOCUS %s purpose=%u hints=%u\n", mode, purpose, hints);
  return FALSE;
}
static void activate(GtkApplication *app, gpointer data) {
  (void)data;
  GtkWidget *window = gtk_application_window_new(app);
  gtk_window_set_decorated(GTK_WINDOW(window), FALSE);
  gtk_window_set_default_size(GTK_WINDOW(window), 650, 450);
  GtkWidget *editor;
  if (g_str_equal(mode, "source")) {
    GtkSourceBuffer *buffer = gtk_source_buffer_new(NULL);
    gtk_source_buffer_begin_not_undoable_action(buffer);
    gtk_text_buffer_set_text(GTK_TEXT_BUFFER(buffer), text, -1);
    gtk_source_buffer_end_not_undoable_action(buffer);
    editor = gtk_source_view_new_with_buffer(buffer);
    g_object_unref(buffer);
  } else {
    editor = gtk_entry_new();
    if (g_str_equal(mode, "password") || g_str_equal(mode, "pin")) {
      gtk_entry_set_input_purpose(GTK_ENTRY(editor), g_str_equal(mode, "pin") ? GTK_INPUT_PURPOSE_PIN : GTK_INPUT_PURPOSE_PASSWORD);
      gtk_entry_set_visibility(GTK_ENTRY(editor), FALSE);
    } else {
      gtk_entry_set_text(GTK_ENTRY(editor), text);
    }
  }
  g_signal_connect(editor, "focus-in-event", G_CALLBACK(focused), NULL);
  gtk_container_add(GTK_CONTAINER(window), editor);
  gtk_widget_show_all(window);
  gtk_widget_grab_focus(editor);
  g_print("GTK3_NATIVE_EDITOR %s GTK %u.%u.%u GtkSourceView %u.%u.%u\n", mode,
          gtk_get_major_version(), gtk_get_minor_version(), gtk_get_micro_version(),
          gtk_source_get_major_version(), gtk_source_get_minor_version(), gtk_source_get_micro_version());
}
int main(int argc, char **argv) {
  if (argc != 3) return 2;
  mode = argv[1];
  if (!g_file_get_contents(argv[2], &text, NULL, NULL)) return 3;
  GtkApplication *app = gtk_application_new("so.tilo.DictationGtk3Fixture", G_APPLICATION_NON_UNIQUE);
  g_signal_connect(app, "activate", G_CALLBACK(activate), NULL);
  int result = g_application_run(G_APPLICATION(app), 1, argv);
  g_object_unref(app); g_free(text);
  return result;
}

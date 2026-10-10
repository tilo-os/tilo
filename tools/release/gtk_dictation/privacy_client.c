/* SPDX-License-Identifier: AGPL-3.0-only
 * Hardware-isolated qualification client, never installed as an app.
 * All editing remains in standard GTK controls.
 */
#include <gtk/gtk.h>

static GtkWidget *fields[5];
static const char *names[] = {"plain", "password", "pin", "private", "multiline"};

static void entered(GtkEventControllerFocus *controller, gpointer data) {
  (void)controller;
  g_print("GTK_PRIVACY_FOCUSED %s\n", (const char *)data);
}

static void observe_focus(GtkWidget *widget, const char *name) {
  GtkEventController *controller = gtk_event_controller_focus_new();
  g_signal_connect(controller, "enter", G_CALLBACK(entered), (gpointer)name);
  gtk_widget_add_controller(widget, controller);
}

static gboolean describe(gpointer data) {
  GtkWidget *window = data;
  if (!gtk_widget_get_mapped(window)) return G_SOURCE_CONTINUE;
  for (int i = 0; i < 5; i++) {
    graphene_rect_t bounds;
    if (!gtk_widget_compute_bounds(fields[i], window, &bounds)) return G_SOURCE_CONTINUE;
    g_print("GTK_PRIVACY_BOUNDS %s %.3f %.3f %.3f %.3f\n", names[i],
            bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height);
  }
  g_print("GTK_PRIVACY_ROOT %d %d\n", gtk_widget_get_width(window), gtk_widget_get_height(window));
  return G_SOURCE_REMOVE;
}

static void activate(GtkApplication *application, gpointer data) {
  (void)data;
  GtkWidget *window = gtk_application_window_new(application);
  gtk_window_set_title(GTK_WINDOW(window), "Isolated GTK input qualification");
  gtk_window_set_default_size(GTK_WINDOW(window), 640, 480);
  gtk_window_set_decorated(GTK_WINDOW(window), FALSE);
  GtkWidget *box = gtk_box_new(GTK_ORIENTATION_VERTICAL, 16);
  gtk_widget_set_margin_top(box, 16); gtk_widget_set_margin_bottom(box, 16);
  gtk_widget_set_margin_start(box, 16); gtk_widget_set_margin_end(box, 16);
  gtk_window_set_child(GTK_WINDOW(window), box);
  for (int i = 0; i < 4; i++) {
    fields[i] = gtk_entry_new();
    gtk_widget_set_size_request(fields[i], -1, 56);
    gtk_entry_set_placeholder_text(GTK_ENTRY(fields[i]), names[i]);
    observe_focus(fields[i], names[i]);
    gtk_box_append(GTK_BOX(box), fields[i]);
  }
  gtk_entry_set_input_purpose(GTK_ENTRY(fields[1]), GTK_INPUT_PURPOSE_PASSWORD);
  gtk_entry_set_visibility(GTK_ENTRY(fields[1]), FALSE);
  gtk_entry_set_input_purpose(GTK_ENTRY(fields[2]), GTK_INPUT_PURPOSE_PIN);
  gtk_entry_set_visibility(GTK_ENTRY(fields[2]), FALSE);
  gtk_entry_set_input_hints(GTK_ENTRY(fields[3]), GTK_INPUT_HINT_PRIVATE);
  /* A nonempty public fixture makes native Copy an observable buffer read:
   * GTK intentionally leaves the old clipboard when an empty field is copied. */
  gtk_editable_set_text(GTK_EDITABLE(fields[3]), "private fixture");
  fields[4] = gtk_text_view_new();
  observe_focus(fields[4], names[4]);
  gtk_widget_set_vexpand(fields[4], TRUE);
  gtk_box_append(GTK_BOX(box), fields[4]);
  gtk_window_present(GTK_WINDOW(window));
  g_timeout_add(100, describe, window);
}

int main(int argc, char **argv) {
  GtkApplication *application = gtk_application_new("so.tilo.DictationPrivacyFixture", G_APPLICATION_NON_UNIQUE);
  g_signal_connect(application, "activate", G_CALLBACK(activate), NULL);
  int result = g_application_run(G_APPLICATION(application), argc, argv);
  g_object_unref(application);
  return result;
}

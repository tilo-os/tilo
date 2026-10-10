/* SPDX-License-Identifier: AGPL-3.0-only
 * Public-data native subject. No input-method replacement or history shim.
 */
#include <gtk/gtk.h>

static void activate (GtkApplication *app, gpointer unused)
{
  GtkWidget *window = gtk_application_window_new (app);
  GtkWidget *entry = gtk_entry_new ();
  (void) unused;
  if (!g_object_class_find_property (G_OBJECT_GET_CLASS (entry), "enable-undo"))
    g_error ("The exact candidate GTK3 history runtime is required");
  gtk_window_set_title (GTK_WINDOW (window), "Gtk3 Editor");
  gtk_window_set_default_size (GTK_WINDOW (window), 600, 160);
  gtk_entry_set_text (GTK_ENTRY (entry), "alpha beta gamma");
  g_object_set (entry, "enable-undo", TRUE, NULL);
  gtk_container_add (GTK_CONTAINER (window), entry);
  gtk_widget_show_all (window);
  gtk_widget_grab_focus (entry);
  gtk_editable_select_region (GTK_EDITABLE (entry), 6, 10);
  g_print ("GTK3_ENTRY_HISTORY_SUBJECT %u.%u.%u stock opt-in\n",
           gtk_get_major_version (), gtk_get_minor_version (), gtk_get_micro_version ());
}

int main (int argc, char **argv)
{
  GtkApplication *app = gtk_application_new ("so.tilo.Gtk3HistoryReview", G_APPLICATION_NON_UNIQUE);
  int status;
  g_signal_connect (app, "activate", G_CALLBACK (activate), NULL);
  status = g_application_run (G_APPLICATION (app), argc, argv);
  g_object_unref (app);
  return status;
}

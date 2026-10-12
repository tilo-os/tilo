/* SPDX-License-Identifier: AGPL-3.0-only
 * Public-data native GtkEntry/AtkAction qualification. No editing override.
 */
#include <gtk/gtk.h>
#include <string.h>
static GtkWidget *entry;
static GtkWidget *other;
static GtkApplication *application;
static gint64 deadline;
static guint intercepted;
static gboolean awaiting_commit;
static GtkEntryBuffer *before_commit;
static gboolean flag (const char *name)
{
  gboolean value = FALSE;
  g_object_get (entry, name, &value, NULL);
  return value;
}
static void attr (AtkObject *object, const char *name, const char *expected)
{
  AtkAttributeSet *set = atk_object_get_attributes (object);
  gboolean found = FALSE;
  for (GSList *p = set; p; p = p->next)
    {
      AtkAttribute *item = p->data;
      if (g_str_equal (item->name, name))
        { g_assert_cmpstr (item->value, ==, expected); found = TRUE; }
    }
  atk_attribute_set_free (set);
  g_assert_true (found);
}
static gboolean intercept (GtkEntry *object, gpointer unused)
{
  (void) object; (void) unused; intercepted++; return FALSE;
}
static void baseline (void)
{
  gtk_entry_set_text (GTK_ENTRY (entry), "alpha beta gamma");
  gtk_editable_select_region (GTK_EDITABLE (entry), 6, 10);
}
static void insert (void)
{
  gint position = 10;
  gtk_editable_insert_text (GTK_EDITABLE (entry), "!", 1, &position);
  g_assert_cmpstr (gtk_entry_get_text (GTK_ENTRY (entry)), ==, "alpha beta! gamma");
}
static gboolean gate (gpointer unused)
{
  AtkObject *object;
  AtkAction *action;
  gint start, end;
  gulong handler;
  GtkEntryBuffer *original;
  (void) unused;
  if (!gtk_widget_has_focus (entry))
    {
      g_assert_cmpint (g_get_monotonic_time (), <, deadline);
      return G_SOURCE_CONTINUE;
    }
  object = gtk_widget_get_accessible (entry); g_assert_true (ATK_IS_ACTION (object));
  action = ATK_ACTION (object);
  if (!awaiting_commit)
    {
  g_assert_cmpstr (atk_action_get_name (action, 0), ==, "activate");
  g_assert_cmpint (atk_action_get_n_actions (action), ==, 1);
  g_assert_false (flag ("history-registered"));
  g_assert_false (gtk_bindings_activate (G_OBJECT (entry), GDK_KEY_z, GDK_CONTROL_MASK));
  g_object_set (entry, "enable-undo", TRUE, NULL);
  baseline (); insert (); g_assert_false (flag ("can-undo"));
  g_assert_false (atk_action_do_action (action, 1));
  g_object_set (entry, "history-owner", "so.tilo.NativeHistoryGate", NULL);
  g_assert_true (flag ("history-registered")); g_assert_true (flag ("history-available"));
  g_assert_cmpint (atk_action_get_n_actions (action), ==, 3);
  g_assert_cmpstr (atk_action_get_name (action, 1), ==, "undo");
  g_assert_cmpstr (atk_action_get_name (action, 2), ==, "redo");
      baseline ();
      before_commit = g_object_ref (gtk_entry_get_buffer (GTK_ENTRY (entry)));
      awaiting_commit = TRUE;
      g_print ("GTK3_NATIVE_GATE_READY_IM_REPLACEMENT\n");
      return G_SOURCE_CONTINUE;
    }
  if (!g_str_equal (gtk_entry_get_text (GTK_ENTRY (entry)), "alpha ! gamma"))
    {
      g_assert_cmpint (g_get_monotonic_time (), <, deadline);
      return G_SOURCE_CONTINUE;
    }
  original = gtk_entry_get_buffer (GTK_ENTRY (entry));
  g_assert_true (original == before_commit);
  attr (object, "tilo-can-undo", "true");
  g_assert_true (atk_action_do_action (action, 1));
  g_assert_cmpstr (gtk_entry_get_text (GTK_ENTRY (entry)), ==, "alpha beta gamma");
  g_assert_true (gtk_editable_get_selection_bounds (GTK_EDITABLE (entry), &start, &end));
  g_assert_cmpint (start, ==, 6); g_assert_cmpint (end, ==, 10);
  g_assert_true (atk_action_do_action (action, 2));
  g_assert_cmpstr (gtk_entry_get_text (GTK_ENTRY (entry)), ==, "alpha ! gamma");
  g_assert_true (original == gtk_entry_get_buffer (GTK_ENTRY (entry)));
  g_print ("PASS native ATK undo/redo and original selection; same stock buffer\n");
  gtk_editable_set_editable (GTK_EDITABLE (entry), FALSE);
  g_assert_false (flag ("history-available")); g_assert_false (atk_action_do_action (action, 1));
  g_assert_false (gtk_bindings_activate (G_OBJECT (entry), GDK_KEY_z, GDK_CONTROL_MASK));
  gtk_editable_set_editable (GTK_EDITABLE (entry), TRUE);
  gtk_widget_set_sensitive (entry, FALSE);
  g_assert_false (flag ("history-available")); g_assert_false (atk_action_do_action (action, 1));
  g_assert_false (gtk_bindings_activate (G_OBJECT (entry), GDK_KEY_z, GDK_CONTROL_MASK));
  gtk_widget_set_sensitive (entry, TRUE); gtk_widget_grab_focus (entry);
  gtk_entry_set_visibility (GTK_ENTRY (entry), FALSE);
  attr (object, "tilo-can-undo", "false"); g_assert_false (atk_action_do_action (action, 1));
  gtk_entry_set_visibility (GTK_ENTRY (entry), TRUE);
  gtk_entry_set_input_purpose (GTK_ENTRY (entry), GTK_INPUT_PURPOSE_PASSWORD);
  g_assert_false (flag ("history-available")); g_assert_false (atk_action_do_action (action, 1));
  g_assert_false (gtk_bindings_activate (G_OBJECT (entry), GDK_KEY_z, GDK_CONTROL_MASK));
  gtk_entry_set_input_purpose (GTK_ENTRY (entry), GTK_INPUT_PURPOSE_PIN);
  g_assert_false (flag ("history-available")); g_assert_false (atk_action_do_action (action, 2));
  gtk_entry_set_input_purpose (GTK_ENTRY (entry), GTK_INPUT_PURPOSE_FREE_FORM);
  gtk_widget_grab_focus (other);
  g_assert_false (flag ("history-available")); g_assert_false (atk_action_do_action (action, 1));
  g_assert_false (gtk_bindings_activate (G_OBJECT (entry), GDK_KEY_z, GDK_CONTROL_MASK));
  gtk_widget_grab_focus (entry); baseline (); insert ();
  handler = g_signal_connect (entry, "tilo-native-undo", G_CALLBACK (intercept), NULL);
  g_assert_false (flag ("history-registered")); g_assert_cmpint (atk_action_get_n_actions (action), ==, 1);
  g_assert_false (atk_action_do_action (action, 1)); g_assert_cmpuint (intercepted, ==, 0);
  g_assert_cmpstr (gtk_entry_get_text (GTK_ENTRY (entry)), ==, "alpha beta! gamma");
  g_signal_handler_disconnect (entry, handler);
  g_object_set (entry, "history-owner", NULL, NULL);
  g_assert_false (flag ("history-available")); g_assert_cmpint (atk_action_get_n_actions (action), ==, 1);
  attr (object, "tilo-history-registered", "false");
  g_assert_false (gtk_bindings_activate (G_OBJECT (entry), GDK_KEY_z, GDK_CONTROL_MASK));
  g_print ("PASS unregistered, readonly, insensitive, protected, focus and intercepted history refusal\n");
  g_clear_object (&before_commit);
  g_print ("GTK3_NATIVE_ATK_GATE_COMPLETE\n");
  g_application_quit (G_APPLICATION (application));
  return G_SOURCE_REMOVE;
}
static void activate (GtkApplication *app, gpointer unused)
{
  GtkWidget *window = gtk_application_window_new (app);
  GtkWidget *box = gtk_box_new (GTK_ORIENTATION_VERTICAL, 0);
  (void) unused;
  entry = gtk_entry_new (); other = gtk_entry_new (); application = app;
  gtk_window_set_decorated (GTK_WINDOW (window), FALSE);
  gtk_window_set_default_size (GTK_WINDOW (window), 600, 160);
  gtk_container_add (GTK_CONTAINER (window), box);
  gtk_box_pack_start (GTK_BOX (box), entry, TRUE, TRUE, 0);
  gtk_box_pack_start (GTK_BOX (box), other, TRUE, TRUE, 0);
  gtk_widget_show_all (window); gtk_widget_grab_focus (entry);
  deadline = g_get_monotonic_time () + 10 * G_TIME_SPAN_SECOND;
  g_timeout_add (20, gate, NULL);
}
int main (int argc, char **argv)
{
  GtkApplication *app = gtk_application_new ("so.tilo.NativeHistoryGate", G_APPLICATION_NON_UNIQUE);
  int status;
  g_signal_connect (app, "activate", G_CALLBACK (activate), NULL);
  status = g_application_run (G_APPLICATION (app), argc, argv);
  g_object_unref (app); return status;
}

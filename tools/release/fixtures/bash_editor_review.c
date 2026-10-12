/* SPDX-License-Identifier: AGPL-3.0-only
 * Native feasibility fixture. Not an installed Terminal backend.
 * Uses the GNU Bash/Readline ABI; exposes only synthetic test input. No
 * producer authentication, finite leases, queryable transaction receipts, focus
 * binding or system Undo integration. Compile only for the isolated review
 * journey; never install this module.
 */
#define _GNU_SOURCE 1
#include "builtins.h"
#include <fcntl.h>
#include <poll.h>
#include <readline/readline.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <unistd.h>
extern int executing;
extern int executing_builtin;
extern void initialize_readline(void);
static int channel = -1, active;
static uint64_t epoch = 1;
static rl_hook_func_t *previous_event;
static rl_vintfunc_t *previous_prep;
static rl_voidfunc_t *previous_deprep, *previous_display;
static rl_getc_func_t *previous_getc;
static uint64_t undo_epoch;
static int saved_point, saved_mark, saved_active;
static int safe(void) {
  int waiting = 0;
  unsigned long bad = RL_STATE_DONE | RL_STATE_MOREINPUT | RL_STATE_ISEARCH |
                      RL_STATE_NSEARCH | RL_STATE_SEARCH | RL_STATE_MACROINPUT |
                      RL_STATE_COMPLETING | RL_STATE_SIGHANDLER |
                      RL_STATE_MULTIKEY | RL_STATE_VIMOTION | RL_STATE_READSTR;
  return active && !executing && !executing_builtin && !rl_done &&
         !(rl_readline_state & bad) && !rl_pending_input &&
         !rl_executing_macro && rl_instream &&
         ioctl(fileno(rl_instream), FIONREAD, &waiting) == 0 && waiting == 0;
}
static void prep(int meta) {
  epoch++;
  active = 0;
  undo_epoch = 0;
  if (previous_prep)
    previous_prep(meta);
  active = !executing && !executing_builtin;
}
static void deprep(void) {
  active = 0;
  epoch++;
  undo_epoch = 0;
  if (previous_deprep)
    previous_deprep();
}
static int event(void);
static int getc_hook(FILE *stream) {
  while (channel >= 0) {
    struct pollfd fds[2] = {{fileno(stream), POLLIN, 0}, {channel, POLLIN, 0}};
    int ready = poll(fds, 2, 100);
    if (ready < 0)
      break;
    if (fds[0].revents)
      break;
    if (fds[1].revents)
      event();
  }
  epoch++;
  undo_epoch = 0;
  return previous_getc(stream);
}
static void display(void) {
  epoch++;
  undo_epoch = 0;
  if (previous_display)
    previous_display();
}
/* Strict UTF-8 and no controls: protocol text can never submit a command. */
static int text_ok(const unsigned char *s, size_t n) {
  size_t i = 0;
  while (i < n) {
    uint32_t c = s[i++], min = 0;
    int tail = 0;
    if (c < 128) {
      if (c < 32 || c == 127)
        return 0;
      continue;
    }
    if (c >= 0xc2 && c <= 0xdf) {
      c &= 31;
      tail = 1;
      min = 128;
    } else if (c >= 0xe0 && c <= 0xef) {
      c &= 15;
      tail = 2;
      min = 2048;
    } else if (c >= 0xf0 && c <= 0xf4) {
      c &= 7;
      tail = 3;
      min = 65536;
    } else
      return 0;
    while (tail--) {
      if (i >= n || (s[i] & 0xc0) != 0x80)
        return 0;
      c = (c << 6) | (s[i++] & 63);
    }
    if (c < min || c > 0x10ffff || (c >= 0xd800 && c <= 0xdfff) ||
        (c >= 128 && c <= 159))
      return 0;
  }
  return 1;
}
static int event(void) {
  char request[4096], response[8192];
  if (previous_event)
    previous_event();
  ssize_t n =
      recv(channel, request, sizeof(request) - 1, MSG_DONTWAIT | MSG_TRUNC);
  if (n < 0)
    return 0;
  if (n == 0 || n >= (ssize_t)sizeof(request)) {
    close(channel);
    channel = -1;
    rl_event_hook = previous_event;
    return 0;
  }
  request[n] = 0;
  if (!safe()) {
    strcpy(response, "UNAVAILABLE\n");
  } else if (!strcmp(request, "GET")) {
    int mark = rl_mark_active_p();
    int start = mark && rl_mark < rl_point ? rl_mark : rl_point;
    int end = mark && rl_mark > rl_point ? rl_mark : rl_point;
    if (rl_end > 4096 || !text_ok((unsigned char *)rl_line_buffer, rl_end))
      strcpy(response, "UNAVAILABLE\n");
    else
      snprintf(response, sizeof(response), "STATE %llu %d %d %d\n%s",
               (unsigned long long)epoch, start, end, rl_end, rl_line_buffer);
  } else {
    unsigned long long expected = 0;
    int start = -1, end = -1, offset = 0;
    if (sscanf(request, "REPLACE %llu %d %d%n", &expected, &start, &end,
               &offset) == 3 &&
        offset > 0 && request[offset] == '\n') {
      int mark = rl_mark_active_p(),
          actual_start = mark && rl_mark < rl_point ? rl_mark : rl_point;
      int actual_end = mark && rl_mark > rl_point ? rl_mark : rl_point;
      offset++;
      size_t bytes = n - offset;
      if (expected != epoch || start != actual_start || end != actual_end)
        strcpy(response, "STALE\n");
      else if (start < 0 || end > rl_end ||
               !text_ok((unsigned char *)request + offset, bytes) ||
               (start < rl_end &&
                ((unsigned char)rl_line_buffer[start] & 0xc0) == 0x80) ||
               (end < rl_end &&
                ((unsigned char)rl_line_buffer[end] & 0xc0) == 0x80) ||
               rl_end - (end - start) + bytes > 4096)
        strcpy(response, "INVALID\n");
      else {
        saved_point = rl_point;
        saved_mark = rl_mark;
        saved_active = mark;
        rl_begin_undo_group();
        rl_delete_text(start, end);
        rl_point = start;
        rl_insert_text(request + offset);
        rl_end_undo_group();
        rl_deactivate_mark();
        if (previous_display)
          previous_display();
        epoch++;
        undo_epoch = epoch;
        snprintf(response, sizeof(response), "APPLIED %llu\n",
                 (unsigned long long)epoch);
      }
    } else if (sscanf(request, "UNDO %llu", &expected) == 1 &&
               expected == epoch && undo_epoch == epoch) {
      if (rl_do_undo()) {
        rl_point = saved_point;
        rl_mark = saved_mark;
        if (saved_active)
          rl_activate_mark();
        else
          rl_deactivate_mark();
        if (previous_display)
          previous_display();
        epoch++;
        undo_epoch = 0;
        snprintf(response, sizeof(response), "UNDONE %llu\n",
                 (unsigned long long)epoch);
      } else
        strcpy(response, "FAILED\n");
    } else
      strcpy(response, "STALE\n");
  }
  if (send(channel, response, strlen(response), MSG_DONTWAIT | MSG_NOSIGNAL) <
      0) {
    close(channel);
    channel = -1;
    rl_event_hook = previous_event;
  }
  return 0;
}
static int init(WORD_LIST *args) {
  if (channel != -1 || !args || args->next)
    return 1;
  char *end;
  long fd = strtol(args->word->word, &end, 10);
  int type = 0;
  socklen_t size = sizeof(type);
  if (*end || fd < 3 || fd > 1024 ||
      getsockopt(fd, SOL_SOCKET, SO_TYPE, &type, &size) ||
      type != SOCK_SEQPACKET)
    return 1;
  if (fcntl(fd, F_SETFD, FD_CLOEXEC))
    return 1;
  initialize_readline();
  channel = fd;
  previous_event = rl_event_hook;
  previous_getc = rl_getc_function;
  previous_prep = rl_prep_term_function;
  previous_deprep = rl_deprep_term_function;
  previous_display = rl_redisplay_function;
  rl_getc_function = getc_hook;
  rl_prep_term_function = prep;
  rl_deprep_term_function = deprep;
  rl_redisplay_function = display;
  return 0;
}
static char *const doc[] = {"Private synthetic editor feasibility probe.",
                            NULL};
struct builtin tilo_review_struct = {
    "tilo_review", init, BUILTIN_ENABLED, doc, "tilo_review FD", NULL};

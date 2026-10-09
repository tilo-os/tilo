/* SPDX-License-Identifier: AGPL-3.0-only
 * GNU Bash/Readline producer for the admitted ush editor capability.
 * No ambient listener, PTY injection, clipboard, microphone or durable text.
 * The supervisor must complete QPE1 and admit this exact original launch.
 */
#define _GNU_SOURCE 1
#include "builtins.h"
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <readline/readline.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/random.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

enum { HEADER = 80, TEXT = 4096, FRAME = HEADER + TEXT };
enum {
  UNAVAILABLE = 1,
  READY,
  APPLIED,
  UNDONE,
  REDONE,
  STALE,
  INVALID,
  UNKNOWN,
  REVOKED
};
#ifndef TILO_BASH_VERSION
#error "build through native/build.py to bind the producer to its Bash build"
#endif
#ifndef TILO_BASH_PATCH
#error "Bash patchlevel is required"
#endif
extern char *dist_version;
extern int patch_level;
extern int executing, executing_builtin;
extern void initialize_readline(void);
static int channel = -1, active, phase;
static uint64_t generation = 1, revision = 1, checkpoint, checkpoint_revision;
static uint64_t lease_revision, lease_expiry;
static unsigned char incarnation[16], last_request[FRAME], last_reply[HEADER];
static size_t last_size;
static char before[TEXT + 1], after[TEXT + 1];
static int before_point, before_mark, before_active, after_point;
static rl_hook_func_t *old_event;
static rl_getc_func_t *old_getc;
static rl_vintfunc_t *old_prep;
static rl_voidfunc_t *old_deprep, *old_display;
static void prep(int), deprep(void), display(void);
static int getc_hook(FILE *);

static uint64_t number(const unsigned char *p, unsigned bytes) {
  uint64_t value = 0;
  for (unsigned i = 0; i < bytes; i++)
    value |= (uint64_t)p[i] << (i * 8);
  return value;
}
static void put(unsigned char *p, uint64_t value, unsigned bytes) {
  for (unsigned i = 0; i < bytes; i++)
    p[i] = value >> (i * 8);
}
static uint64_t now_ns(void) {
  struct timespec t;
  if (clock_gettime(CLOCK_MONOTONIC, &t) || t.tv_sec < 0 ||
      (uint64_t)t.tv_sec > (UINT64_MAX - 120000000000ULL) / 1000000000ULL)
    return 0;
  return (uint64_t)t.tv_sec * 1000000000ULL + (uint64_t)t.tv_nsec;
}
static void retire(void) {
  active = 0;
  phase = 0;
  memset(before, 0, sizeof(before));
  memset(after, 0, sizeof(after));
  if (channel >= 0) {
    shutdown(channel, SHUT_RDWR);
    close(channel);
    channel = -1;
  }
}
static int bump(uint64_t *value) {
  if (*value >= UINT64_MAX - 1) {
    retire();
    return 0;
  }
  ++*value;
  return 1;
}
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
static int boundary(unsigned position) {
  return position <= (unsigned)rl_end &&
         (position == (unsigned)rl_end ||
          ((unsigned char)rl_line_buffer[position] & 0xc0) != 0x80);
}
static int safe(void) {
  int waiting = 0;
  unsigned long bad = RL_STATE_DONE | RL_STATE_MOREINPUT | RL_STATE_ISEARCH |
                      RL_STATE_NSEARCH | RL_STATE_SEARCH | RL_STATE_MACROINPUT |
                      RL_STATE_COMPLETING | RL_STATE_SIGHANDLER |
                      RL_STATE_MULTIKEY | RL_STATE_VIMOTION | RL_STATE_READSTR;
  return channel >= 0 && active && !executing && !executing_builtin &&
         !rl_done && generation < UINT64_MAX && revision < UINT64_MAX &&
         !(rl_readline_state & bad) && !rl_pending_input &&
         !rl_executing_macro && rl_getc_function == getc_hook &&
         rl_prep_term_function == prep && rl_deprep_term_function == deprep &&
         rl_redisplay_function == display && rl_event_hook == old_event &&
         rl_instream && rl_line_buffer && rl_end >= 0 && rl_end <= TEXT &&
         rl_point >= 0 && boundary(rl_point) &&
         (!rl_mark_active_p() || (rl_mark >= 0 && boundary(rl_mark))) &&
         text_ok((unsigned char *)rl_line_buffer, rl_end) &&
         ioctl(fileno(rl_instream), FIONREAD, &waiting) == 0 && waiting == 0;
}
static void header(unsigned char *frame, unsigned status, uint64_t id,
                   uint64_t transaction) {
  memset(frame, 0, HEADER);
  memcpy(frame, "TBE1", 4);
  frame[4] = 1;
  frame[6] = status;
  put(frame + 8, id, 8);
  put(frame + 64, transaction, 8);
}
static int send_frame(const unsigned char *frame, size_t length) {
  if (channel < 0)
    return 0;
  if (send(channel, frame, length, MSG_DONTWAIT | MSG_NOSIGNAL) !=
      (ssize_t)length) {
    retire();
    return 0;
  }
  return 1;
}
static void publish(uint64_t id) {
  unsigned char frame[FRAME];
  header(frame, UNAVAILABLE, id, 0);
  uint64_t now = now_ns();
  size_t length = HEADER;
  if (safe() && now) {
    if (lease_revision == revision && now >= lease_expiry)
      bump(&revision);
    if (lease_revision != revision) {
      lease_revision = revision;
      lease_expiry = now + 120000000000ULL;
    }
    frame[6] = READY;
    frame[7] = 4 | ((phase == 1 && checkpoint_revision == revision) ? 1 : 0) |
               ((phase == 2 && checkpoint_revision == revision) ? 2 : 0);
    memcpy(frame + 16, incarnation, 16);
    put(frame + 32, generation, 8);
    put(frame + 40, revision, 8);
    put(frame + 48, lease_expiry, 8);
    put(frame + 56, rl_mark_active_p() ? rl_mark : rl_point, 4);
    put(frame + 60, rl_point, 4);
    put(frame + 72, rl_end, 4);
    memcpy(frame + HEADER, rl_line_buffer, rl_end);
    length += rl_end;
  }
  send_frame(frame, length);
}
static void prep(int meta) {
  active = 0;
  phase = 0;
  bump(&generation);
  bump(&revision);
  if (old_prep)
    old_prep(meta);
  active = !executing && !executing_builtin;
}
static void deprep(void) {
  active = 0;
  phase = 0;
  bump(&revision);
  publish(0);
  if (old_deprep)
    old_deprep();
}
static void display(void) {
  phase = 0;
  bump(&revision);
  if (old_display)
    old_display();
}
static int same_lease(const unsigned char *request) {
  uint64_t expiry = number(request + 48, 8), now = now_ns();
  return safe() && now && now < expiry && expiry - now <= 120000000000ULL &&
         lease_revision == revision && expiry == lease_expiry &&
         !memcmp(request + 16, incarnation, 16) &&
         number(request + 32, 8) == generation &&
         number(request + 40, 8) == revision &&
         number(request + 56, 4) ==
             (unsigned)(rl_mark_active_p() ? rl_mark : rl_point) &&
         number(request + 60, 4) == (unsigned)rl_point;
}
static void remember(const unsigned char *request, size_t size,
                     const unsigned char *response) {
  memcpy(last_request, request, size);
  last_size = size;
  memcpy(last_reply, response, HEADER);
}
static void service(void) {
  unsigned char request[FRAME], response[HEADER];
  ssize_t received =
      recv(channel, request, sizeof(request), MSG_DONTWAIT | MSG_TRUNC);
  if (received < 0) {
    if (errno != EAGAIN && errno != EINTR)
      retire();
    return;
  }
  if (received < HEADER || received > FRAME) {
    retire();
    return;
  }
  size_t bytes = received - HEADER;
  uint64_t id = number(request + 8, 8), transaction = number(request + 64, 8);
  unsigned op = request[5];
  if (memcmp(request, "TBE1", 4) || request[4] != 1 || request[6] ||
      request[7] || number(request + 76, 4) ||
      number(request + 72, 4) != bytes || !id || op < 1 || op > 6 ||
      (op != 2 && bytes)) {
    retire();
    return;
  }
  if (op == 1 || op == 6) {
    unsigned fields = 0;
    for (unsigned i = 16; i < 72; i++)
      fields |= request[i];
    if (fields) {
      retire();
      return;
    }
  }
  if (op == 1) {
    publish(id);
    return;
  }
  header(response, INVALID, id, transaction);
  if (op == 6) {
    phase = 0;
    bump(&revision);
    header(response, REVOKED, id, 0);
    send_frame(response, HEADER);
    publish(0);
    return;
  }
  if (!transaction) {
    send_frame(response, HEADER);
    return;
  }
  /* Query observes only an original receipt; it never re-applies an edit. */
  if (op == 5) {
    if (last_size && number(last_request + 8, 8) == transaction &&
        !memcmp(request + 16, last_request + 16, 16)) {
      memcpy(response, last_reply, HEADER);
      put(response + 8, id, 8);
    } else
      response[6] = UNKNOWN;
    send_frame(response, HEADER);
    return;
  }
  if (last_size && number(last_request + 8, 8) == id) {
    if (last_size == (size_t)received &&
        !memcmp(last_request, request, received))
      send_frame(last_reply, HEADER);
    else
      send_frame(response, HEADER);
    return;
  }
  if (!same_lease(request)) {
    response[6] = STALE;
    send_frame(response, HEADER);
    return;
  }
  if (op == 2) {
    unsigned anchor = number(request + 56, 4), focus = number(request + 60, 4);
    unsigned start = anchor < focus ? anchor : focus,
             end = anchor > focus ? anchor : focus;
    if (!text_ok(request + HEADER, bytes) || !boundary(start) ||
        !boundary(end) || (size_t)rl_end - (end - start) + bytes > TEXT) {
      send_frame(response, HEADER);
      return;
    }
    memcpy(before, rl_line_buffer, rl_end + 1);
    before_point = rl_point;
    before_mark = rl_mark;
    before_active = rl_mark_active_p();
    char expected[TEXT + 1];
    memcpy(expected, before, start);
    memcpy(expected + start, request + HEADER, bytes);
    memcpy(expected + start + bytes, before + end, strlen(before) - end + 1);
    char text[TEXT + 1];
    memcpy(text, request + HEADER, bytes);
    text[bytes] = 0;
    rl_begin_undo_group();
    rl_delete_text(start, end);
    rl_point = start;
    rl_insert_text(text);
    rl_end_undo_group();
    rl_deactivate_mark();
    if (strcmp(rl_line_buffer, expected)) {
      retire();
      return; /* uncertain: never acknowledge a partial native insertion */
    }
    memcpy(after, rl_line_buffer, rl_end + 1);
    after_point = rl_point;
    checkpoint = transaction;
    phase = 1;
    response[6] = APPLIED;
  } else if (transaction != checkpoint || checkpoint_revision != revision) {
    response[6] = STALE;
    send_frame(response, HEADER);
    return;
  } else if (op == 3 && phase == 1) {
    if (!rl_do_undo() || strcmp(rl_line_buffer, before)) {
      retire();
      return; /* uncertain, never fabricate a successful Undo */
    }
    rl_point = before_point;
    rl_mark = before_mark;
    if (before_active)
      rl_activate_mark();
    else
      rl_deactivate_mark();
    phase = 2;
    response[6] = UNDONE;
  } else if (op == 4 && phase == 2) {
    rl_begin_undo_group();
    rl_delete_text(0, rl_end);
    rl_point = 0;
    rl_insert_text(after);
    rl_point = after_point;
    rl_end_undo_group();
    rl_deactivate_mark();
    if (strcmp(rl_line_buffer, after)) {
      retire();
      return; /* uncertain: no fabricated Redo acknowledgement */
    }
    phase = 1;
    response[6] = REDONE;
  } else {
    response[6] = STALE;
    send_frame(response, HEADER);
    return;
  }
  bump(&revision);
  checkpoint_revision = revision;
  if (old_display)
    old_display();
  remember(request, received, response);
  if (send_frame(response, HEADER))
    publish(0);
}
static int getc_hook(FILE *stream) {
  publish(0);
  while (channel >= 0) {
    struct pollfd fds[2] = {{fileno(stream), POLLIN, 0}, {channel, POLLIN, 0}};
    int ready = poll(fds, 2, -1);
    if (ready < 0 || fds[0].revents)
      break;
    if (fds[1].revents)
      service();
  }
  phase = 0;
  bump(&revision);
  return old_getc(stream);
}
static int wait_input(void) {
  struct pollfd p = {channel, POLLIN, 0};
  return poll(&p, 1, 2000) == 1 && p.revents == POLLIN;
}
static int bind_process(void) {
  unsigned char magic[4], challenge[32], ready;
  if (!wait_input() || recv(channel, magic, sizeof(magic), MSG_TRUNC) != 4 ||
      memcmp(magic, "QPE1", 4) || !wait_input() ||
      recv(channel, challenge, sizeof(challenge), MSG_TRUNC) != 32)
    return 0;
  int proof[2];
  if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, proof))
    return 0;
  union {
    struct cmsghdr align;
    unsigned char bytes[CMSG_SPACE(sizeof(int))];
  } control;
  memset(&control, 0, sizeof(control));
  struct iovec iov = {challenge, sizeof(challenge)};
  struct msghdr message = {0};
  message.msg_iov = &iov;
  message.msg_iovlen = 1;
  message.msg_control = control.bytes;
  message.msg_controllen = sizeof(control.bytes);
  struct cmsghdr *header = CMSG_FIRSTHDR(&message);
  header->cmsg_level = SOL_SOCKET;
  header->cmsg_type = SCM_RIGHTS;
  header->cmsg_len = CMSG_LEN(sizeof(int));
  memcpy(CMSG_DATA(header), &proof[1], sizeof(int));
  int sent = sendmsg(channel, &message, MSG_NOSIGNAL) == 32;
  close(proof[0]);
  close(proof[1]);
  return sent && wait_input() && recv(channel, &ready, 1, MSG_TRUNC) == 1 &&
         ready == 1;
}
static int init(WORD_LIST *args) {
  if (channel >= 0 || !args || args->next)
    return 1;
  char *end;
  errno = 0;
  long fd = strtol(args->word->word, &end, 10);
  int type;
  socklen_t size = sizeof(type);
  if (errno || *end || fd < 3 || fd > 1048576 ||
      getsockopt(fd, SOL_SOCKET, SO_TYPE, &type, &size) ||
      type != SOCK_SEQPACKET || fcntl(fd, F_SETFD, FD_CLOEXEC))
    return 1;
  channel = fd;
  if (getrandom(incarnation, sizeof(incarnation), 0) !=
          (ssize_t)sizeof(incarnation) ||
      !bind_process()) {
    retire();
    return 1;
  }
  unsigned random_bits = 0;
  for (unsigned i = 0; i < sizeof(incarnation); i++)
    random_bits |= incarnation[i];
  if (!random_bits) {
    retire();
    return 1;
  }
  if (strcmp(dist_version, TILO_BASH_VERSION) ||
      patch_level != TILO_BASH_PATCH ||
      rl_readline_version != RL_READLINE_VERSION) {
    retire();
    return 1;
  }
  initialize_readline();
  if (rl_event_hook || rl_getc_function != rl_getc) {
    retire();
    return 1;
  }
  old_event = rl_event_hook;
  old_getc = rl_getc_function;
  old_prep = rl_prep_term_function;
  old_deprep = rl_deprep_term_function;
  old_display = rl_redisplay_function;
  rl_getc_function = getc_hook;
  rl_prep_term_function = prep;
  rl_deprep_term_function = deprep;
  rl_redisplay_function = display;
  return 0;
}
void tilo_editable_builtin_unload(char *name) {
  (void)name;
  retire();
  if (rl_getc_function == getc_hook)
    rl_getc_function = old_getc;
  if (rl_prep_term_function == prep)
    rl_prep_term_function = old_prep;
  if (rl_deprep_term_function == deprep)
    rl_deprep_term_function = old_deprep;
  if (rl_redisplay_function == display)
    rl_redisplay_function = old_display;
}
static char *const doc[] = {"Admitted ush native editor capability.", NULL};
struct builtin tilo_editable_struct = {
    "tilo_editable", init, BUILTIN_ENABLED, doc, "tilo_editable FD", NULL};

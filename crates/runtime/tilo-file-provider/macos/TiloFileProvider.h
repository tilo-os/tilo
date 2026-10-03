// The C bridge (crates/runtime/tilo-file-provider/src/lib.rs) the File
// Provider extension reads the runtime's workspace files through. Every call
// but open and close answers JSON, {"ok": ...} or {"error": "..."}, freed
// with tilo_fp_string_free.
#pragma once

typedef struct TiloFpSession TiloFpSession;

// socket NULL: the default runtime profile's files.sock.
TiloFpSession *tilo_fp_open(const char *socket);
void tilo_fp_close(TiloFpSession *session);

char *tilo_fp_list(const TiloFpSession *session, const char *path);
char *tilo_fp_stat(const TiloFpSession *session, const char *path);
char *tilo_fp_fetch(const TiloFpSession *session, const char *path, const char *destination);

void tilo_fp_string_free(char *string);

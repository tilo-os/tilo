#include <spa/pod/command.h>

uint32_t libspa_rs_command_type(struct spa_command *cmd) {
    return SPA_COMMAND_TYPE(cmd);
}

uint32_t libspa_rs_command_id(struct spa_command *cmd, uint32_t type) {
    return SPA_COMMAND_ID(cmd, type);
}

struct spa_command libspa_rs_command_init(uint32_t type, uint32_t id) {
    return SPA_COMMAND_INIT(type, id);
}

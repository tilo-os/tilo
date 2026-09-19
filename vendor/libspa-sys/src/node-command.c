#include <spa/node/command.h>

uint32_t libspa_rs_node_command_id(struct spa_command *cmd) {
    return SPA_NODE_COMMAND_ID(cmd);
}

struct spa_command libspa_rs_node_command_init(uint32_t id) {
    return SPA_NODE_COMMAND_INIT(id);
}

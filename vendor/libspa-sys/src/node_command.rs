use super::*;

extern "C" {
    #[link_name = "libspa_rs_node_command_id"]
    pub fn spa_node_command_id(cmd: *mut spa_command) -> u32;

    #[link_name = "libspa_rs_node_command_init"]
    pub fn spa_node_command_init(id: u32) -> spa_command;
}

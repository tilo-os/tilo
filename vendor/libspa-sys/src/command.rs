use super::*;

extern "C" {
    #[link_name = "libspa_rs_command_type"]
    pub fn spa_command_type(cmd: *mut spa_command) -> u32;

    #[link_name = "libspa_rs_command_id"]
    pub fn spa_command_id(cmd: *mut spa_command, type_: u32) -> u32;

    #[link_name = "libspa_rs_command_init"]
    pub fn spa_command_init(type_: u32, id: u32) -> spa_command;
}

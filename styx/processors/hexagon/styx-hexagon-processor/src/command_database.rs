// SPDX-License-Identifier: BSD-2-Clause

// These are from hw/hexagon/cmd-db.c in QEMU.

use styx_core::prelude::{BuildingProcessor, Peripheral};

const CMD_DB_BASE: u32 = 0x8086_0000;

#[repr(packed)]
pub struct CommandDatabaseHeader {
    version: u32,
    magic: [u8; 4],
    header: [ResourceHeader; 8],
    checksum: u32,
    reserved: u32,
}

#[repr(packed)]
pub struct ResourceHeader {
    slave_id: u16,
    header_offset: u16,
    data_offset: u16,
    count: u16,
    version: u16,
    reserved: [u16; 3],
}

#[repr(packed)]
pub struct EntryHeader {
    id: [u8; 8],
    priority: [u32; 2],
    address: u32,
    len: u16,
    offset: u16,
}

pub struct CommandDatabase;

impl Peripheral for CommandDatabase {
    fn name(&self) -> &str {
        "Command database"
    }
    fn init(&mut self, proc: &mut BuildingProcessor) -> Result<(), UnknownError> {
        proc.core.memory.data().write(CMD_DB_BASE, 1u32);
        proc.core.memory.data().write(CMD_DB_BASE + 4, 0xc0330dbu32);

        Ok(())
    }
}

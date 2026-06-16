// SPDX-License-Identifier: BSD-2-Clause

use std::{collections::HashMap, error::Error};

use bytemuck::{bytes_of, NoUninit};
use styx_core::{
    errors::UnknownError,
    memory::Mmu,
    prelude::{log::info, BuildingProcessor, Context, Peripheral},
};

use crate::HexagonProcessorConfig;

mod soc_info;

// From drivers/soc/qcom/smem.c
// SMEM_BASE_ADDR + OFFSET                      - SmemPartitionHeader
//                                                .......
//                                                .......
//                                                .......
// SMEM_BASE_ADDR + OFFSET + 0x20               - SmemPrivateEntry
//                                                .....
//                                                .....
//                                                .....
//                                                .....
// SMEM_BASE_ADDR + OFFSET + 0x30               - data ..........................
//                                                ...............................
//                                                ...............................
//                                                ...............................
// SMEM_BASE_ADDR + OFFSET + 0x30 + sizeof(data) - SmemPrivateEntry
//                                                 ..........
// and so on.
// SMEM_TOC                                      - SmemInfo
//                                                 ........
//                                                 ........
//                                                 ........
// SMEM_TOC + 0x20                               - SmemPartitionTableEntry
//                                                 ........ -> OFFSET
//                                                 ........
//                                                 ........

#[repr(packed)]
#[derive(NoUninit, Clone, Copy)]
pub struct SmemInfo {
    magic: u32,
    size: u32,
    base_addr: u32,
    reserved: u32,
    num_items: u32,
}
#[repr(packed)]
#[derive(NoUninit, Clone, Copy)]
pub struct SmemPartitionEntryHeader {
    magic: u32,
    host0: u16,
    host1: u16,
    size: u32,
    offset_free_uncached: u32,
    offset_free_cached: u32,
    // make size 0x20
    reserved: [u32; 3],
}

#[repr(packed)]
#[derive(NoUninit, Clone, Copy)]
pub struct SmemPartitionTableHeader {
    magic: u32,
    version: u32,
    num_entries: u32,
    reserved: [u32; 5],
}

#[repr(packed)]
#[derive(NoUninit, Clone, Copy)]
pub struct SmemPartitionTableEntry {
    offset: u32,
    size: u32,
    flags: u32,
    host0: u16,
    host1: u16,
    cacheline: u32,
    reserved: [u32; 7],
}

#[repr(packed)]
#[derive(NoUninit, Clone, Copy)]
pub struct SmemPrivateEntry {
    canary: u16,
    item: u16,
    size: u32,
    padding_data: u16,
    padding_hdr: u16,
    reserved: u32,
}

pub struct SmemInformation {
    smem_size: u32,
    partitions: Vec<SmemPartition>,
}

pub struct SmemPartition {
    smem_partition_size: u32,
    host0: u16,
    host1: u16,
    entries: Vec<SmemEntry>,
}

pub struct SmemEntry {
    // item number
    item_number: u16,
    // the size in the real struct is rounded up to 0x10 and the offset is set accordingly
    size: u32,
}

#[derive(Debug)]
pub struct SmemItem {
    start_addr: u64,
    size: usize,
}

#[derive(Debug)]
pub struct SmemItemMap(HashMap<u16, SmemItem>);

const SMEM_SIZE: u32 = 0x100000;
const SMEM_GLOBALPART_SIZE: u32 = 0x10000;

// From lk2nd (table of contents magic)
const SMEM_TARGET_INFO_IDENTIFIER: u32 = 0x49494953;
// From linux kernel
const SMEM_PARTITION_TABLE_MAGIC: u32 = 0x434f5424; // $TOC
const SMEM_PARTITION_ENTRY_MAGIC: u32 = 0x54525024; // $PRT
const SMEM_GLOBAL_IDENTIFIER: u16 = 0xfffe;
const SMEM_PARTHEADER_OFFSET: u32 = 0x1000;
const SMEM_PRIVATE_ENTRY_CANARY: u16 = 0xa5a5;

#[derive(Default)]
pub struct Smem {
    base_addr: Option<u32>,
    toc: Option<u32>,
}

impl Smem {
    pub fn write_smem_information(&self, mmu: &mut Mmu, info: &SmemInformation) -> SmemItemMap {
        let base_addr = self.base_addr.unwrap() as u32;
        let toc = self.toc.unwrap();
        let smem_info = SmemInfo {
            magic: SMEM_TARGET_INFO_IDENTIFIER,
            size: info.smem_size,
            base_addr,
            reserved: 0,
            // I guess setting this to zero makes the system auto-compute the length.
            num_items: 0x276, // info.partitions.len() as u32,
        };

        Self::write_smem_struct(base_addr, &smem_info, mmu);

        let mut partition_entries: Vec<SmemPartitionTableEntry> = vec![];
        let partition_entry_start = toc;

        let mut allocation_mappings = SmemItemMap(HashMap::new());

        // write the partition table header
        Self::write_smem_struct(
            toc,
            &SmemPartitionTableHeader {
                magic: SMEM_PARTITION_TABLE_MAGIC,
                version: 1,
                num_entries: info
                    .partitions
                    .len()
                    .try_into()
                    .expect("too many partition entries to fit into u32!!"),
                reserved: [0; 5],
            },
            mmu,
        );

        // write each partition separately
        for (i, partition_entry) in info.partitions.iter().enumerate() {
            let offset = if i == 0 {
                SMEM_PARTHEADER_OFFSET
            } else {
                partition_entries[i - 1].offset + partition_entries[i - 1].size
            };

            partition_entries.push(SmemPartitionTableEntry {
                offset,
                size: info.partitions[i].smem_partition_size,
                flags: 0,
                host0: info.partitions[i].host0,
                host1: info.partitions[i].host1,
                cacheline: 0,
                reserved: [0; 7],
            });

            Self::write_smem_struct(
                (toc as usize
                    + (size_of::<SmemPartitionTableEntry>() * i)
                    + size_of::<SmemPartitionTableHeader>()) as u32,
                &partition_entries[i],
                mmu,
            );

            let header = SmemPartitionEntryHeader {
                magic: SMEM_PARTITION_ENTRY_MAGIC,
                host0: info.partitions[i].host0,
                host1: info.partitions[i].host1,
                size: info.partitions[i].smem_partition_size,
                offset_free_uncached: size_of::<SmemPartitionEntryHeader>() as u32
                    + partition_entry.entries.iter().fold(0, |cur, entry| {
                        cur + (entry.size + size_of::<SmemPrivateEntry>() as u32)
                    }),
                offset_free_cached: info.partitions[i].smem_partition_size,
                reserved: [0; 3],
            };

            info!("writing partition entry header at {:x}", base_addr + offset);
            Self::write_smem_struct(base_addr + offset, &header, mmu);

            let mut current_entry_offset =
                base_addr + offset + size_of::<SmemPartitionEntryHeader>() as u32;
            for entry in partition_entry.entries.iter() {
                let priv_entry = SmemPrivateEntry {
                    canary: SMEM_PRIVATE_ENTRY_CANARY,
                    item: entry.item_number,
                    size: entry.size,
                    padding_data: 0,
                    padding_hdr: 0, // entry.size.try_into().expect("couldn't convert entry.size"),
                    reserved: 0,
                };

                allocation_mappings.0.insert(
                    priv_entry.item,
                    SmemItem {
                        start_addr: current_entry_offset as u64
                            + (size_of::<SmemPrivateEntry>() as u64),
                        size: entry.size as usize,
                    },
                );

                info!("writing private entry header at {:x}", current_entry_offset);
                Self::write_smem_struct(current_entry_offset, &priv_entry, mmu);
                current_entry_offset += entry.size + (size_of::<SmemPrivateEntry>() as u32);
            }
        }

        allocation_mappings
    }

    pub fn write_smem_struct<T>(start: u32, data: &T, mmu: &mut Mmu)
    where
        T: NoUninit,
    {
        let bytes: &[u8] = bytemuck::bytes_of(data);
        for (i, b) in bytes.iter().enumerate() {
            let addr = (start + (i as u32));
            info!("writing smem 0x{addr:x} with byte 0x{b:x}");

            mmu.write_u8_le_phys_data(addr as u64, *b).unwrap()
        }
    }
}

impl Peripheral for Smem {
    fn name(&self) -> &str {
        "Qualcomm Shared MEMory"
    }

    fn init(&mut self, proc: &mut BuildingProcessor) -> Result<(), UnknownError> {
        let proc_cfg = proc.config.get::<HexagonProcessorConfig>().expect("You need to provide a hexagon process configuration in processor config to initialize smem");
        let smem_cfg = &proc_cfg.smem_config;

        // We expect a u32 base physical addr because in various places in Qualcomm code (eg. Linux driver),
        // SMEM addressing is done with 32 bits, which hints that whatever the address is, it should fit in
        // 32 bits.
        self.base_addr = Some(
            smem_cfg
                .base_addr
                .try_into()
                .expect("expect smem base address to fit into u32"),
        );

        // Write base address to where the firmware expects it
        proc.vcpus[0]
            .mmu
            .write_u32_le_phys_data(smem_cfg.smem_addr_read_base, smem_cfg.base_addr as u32)
            .with_context(|| {
                "couldn't write smem base address to location where base address is read from"
            })?;

        self.toc = Some(
            u32::try_from(smem_cfg.base_addr).expect("expect base addr within size u32")
                + SMEM_SIZE
                - 0x1000,
        );

        let result = self.write_smem_information(
            &mut proc.vcpus[0].mmu,
            &SmemInformation {
                smem_size: SMEM_SIZE,
                partitions: vec![
                    SmemPartition {
                        smem_partition_size: SMEM_GLOBALPART_SIZE,
                        host0: SMEM_GLOBAL_IDENTIFIER,
                        host1: SMEM_GLOBAL_IDENTIFIER,
                        entries: vec![SmemEntry {
                            // soc info stuff
                            item_number: 0x89,
                            // the size in the real struct is rounded up to 0x10 and the offset is set accordingly
                            size: 0xb0,
                        }],
                    },
                    SmemPartition {
                        smem_partition_size: SMEM_GLOBALPART_SIZE,
                        host0: 1,
                        host1: 0xe,
                        entries: vec![],
                    },
                ],
            },
        );

        // Write chip info with relevant fields
        for (smem_item_id, allocation_info) in result.0 {
            match smem_item_id {
                // Chip info
                0x89 => soc_info::write_socinfo(
                    allocation_info.start_addr as u32,
                    &mut proc.vcpus[0].mmu,
                ),
                _ => {}
            }
        }

        Ok(())
    }
}

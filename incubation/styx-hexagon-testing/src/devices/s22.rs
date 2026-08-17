// SPDX-License-Identifier: BSD-2-Clause

use log::warn;
use styx_emulator::{
    cpu::{arch::hexagon::HexagonRegister, CpuBackendExt},
    errors::UnknownError,
    hooks::{CoreHandle, StyxHook},
    prelude::{
        log::{info, trace},
        Processor, WriteExt,
    },
    processors::hexagon::hexagon::{HexagonConfigTable, HexagonProcessorConfig, QTimerConfig},
};

use super::HexagonDevice;

#[derive(Default)]
pub struct S22 {}

impl HexagonDevice for S22 {
    fn proc_config(&self) -> Result<HexagonProcessorConfig, UnknownError> {
        info!("S22 processor config");

        Ok(HexagonProcessorConfig {
            hardware_threads: 8,
            config_table: [
                (HexagonConfigTable::L2TCM, 0x540),
                (HexagonConfigTable::L2EcomemSize, 0x800),
                (HexagonConfigTable::L2Config, 0x57a),
                (HexagonConfigTable::Etm, 0x57a),
                (HexagonConfigTable::L2InstructionTCM, 0x560),
                (HexagonConfigTable::Clade1, 0x57d),
                (HexagonConfigTable::Clade2, 0x55b),
            ]
            .into_iter()
            .collect(),
            qtimer_config: QTimerConfig {
                irq: 2,
                pcycles_per_packet: 32000,
                ..Default::default()
            },
            ..Default::default()
        })
    }

    fn hooks(&self) -> Result<Vec<StyxHook>, UnknownError> {
        Ok(vec![
            // This instruction takes far too long, so we must fixing.
            /*
            bc499670  00674014   { p0 = cmp.eq(r0,r7); if (!p0.new) jump:t 0xbc499670;
            bc499674  004400b0     r0 = add(r0,#32);
            bc499678  00c0c0a0     dczeroa(r0); }
            */
            StyxHook::CodeVirtual(
                0xbc499670u64.into(),
                Box::new(|proc: CoreHandle| {
                    info!("playing with pc for long insn");
                    // update r0 to equal r7, since this
                    // packet breaks its loop after r7 is equal to r0
                    let r7 = proc.cpu.read_register::<u32>(HexagonRegister::R7).unwrap();
                    {
                        let this = &mut *proc.cpu;
                        let reg = HexagonRegister::R0;
                        let reg = reg.into();
                        this.write_register_raw(reg, r7.into())
                    }
                    .unwrap();

                    proc.cpu
                        .write_register(HexagonRegister::Pc, 0xbc49967cu32)
                        .unwrap();
                    Ok(())
                }),
            ),
            // quick clade2 test
            StyxHook::MemoryRead(
                (0x120000000..0x140000000).into(),
                Box::new(
                    |proc: CoreHandle, address: u64, size: u32, data: &mut [u8]| {
                        /*    panic!(
                            "clade2 read addr {:x} size {} data {:x?} pc {:x?}",
                            address,
                            size,
                            data,
                            proc.cpu.pc()
                        );*/

                        Ok(())
                    },
                ),
            ),
            // Something related to waipio chipset/revision/whatever. Firmware needs this.
            StyxHook::MemoryRead(
                (0x1fc8000..0x1fc8004).into(),
                Box::new(
                    |proc: CoreHandle, address: u64, size: u32, data: &mut [u8]| {
                        data.copy_from_slice(&0xa001_0000_u32.to_le_bytes());

                        Ok(())
                    },
                ),
            ),
            // smem related
            StyxHook::MemoryRead(
                (0xe00_0000..0xe01_2000).into(),
                Box::new(
                    |proc: CoreHandle,
                     address: u64,
                     size: u32,
                     data: &mut [u8]|
                     -> Result<(), UnknownError> {
                        info!(
                            "SMEM memory read pc {:x?} {address:x} size {size} data {data:?}",
                            proc.cpu.pc()
                        );
                        Ok(())
                    },
                ),
            ),
            StyxHook::MemoryWrite(
                (0xe00_2000..0xe20_0000).into(),
                Box::new(
                    |proc: CoreHandle,
                     address: u64,
                     size: u32,
                     data: &[u8]|
                     -> Result<(), UnknownError> {
                        info!(
                            "SMEM memory write pc {:x?} {address:x} size {size} data {data:?}",
                            proc.cpu.pc()
                        );
                        Ok(())
                    },
                ),
            ),
            StyxHook::MemoryRead(
                (0x1fc8000..0x1fe8000).into(),
                Box::new(
                    |proc: CoreHandle,
                     address: u64,
                     size: u32,
                     data: &mut [u8]|
                     -> Result<(), UnknownError> {
                        info!(
                            "SMEMPre memory read pc {:x?} {address:x} size {size} data {data:?}",
                            proc.cpu.pc()
                        );
                        Ok(())
                    },
                ),
            ),
            StyxHook::MemoryWrite(
                (0x1fc8000..0x1fe8000).into(),
                Box::new(
                    |proc: CoreHandle,
                     address: u64,
                     size: u32,
                     data: &[u8]|
                     -> Result<(), UnknownError> {
                        info!(
                            "SMEMPre memory write pc {:x?} {address:x} size {size} data {data:?}",
                            proc.cpu.pc()
                        );
                        Ok(())
                    },
                ),
            ),
            StyxHook::MemoryReadVirtual(
                (0xeb1ae000..(0xeb1ae000 + 0x2000)).into(),
                Box::new(
                    |proc: CoreHandle,
                     address: u64,
                     size: u32,
                     data: &mut [u8]|
                     -> Result<(), UnknownError> {
                        warn!(
                            "mpss_pll read pc {:x?} {address:x} data {data:x?}",
                            proc.cpu.pc()
                        );
                        if address == 0xeb1ae000 {
                            let b = 0x8000_0000_u32.to_le_bytes();
                            for i in 0..data.len() {
                                data[i] = b[i];
                            }
                        } else if address == 0xeb1af424 {
                            warn!("this is probably not mpss pll but...");

                            data.copy_from_slice(&[0, 0, 0, 0]);
                        }
                        Ok(())
                    },
                ),
            ),
            StyxHook::MemoryWriteVirtual(
                (0xeb1ae000..(0xeb1ae000 + 0x2000)).into(),
                Box::new(
                    |proc: CoreHandle,
                     address: u64,
                     size: u32,
                     data: &[u8]|
                     -> Result<(), UnknownError> {
                        warn!(
                            "mpss_pll read pc {:x?} write {address:x} data {data:x?}",
                            proc.cpu.pc()
                        );
                        Ok(())
                    },
                ),
            ),
            // rsc for real
            StyxHook::MemoryReadVirtual(
                (0xeb20_0000..(0xeb20_0000 + 0xa0000)).into(),
                Box::new(
                    |proc: CoreHandle,
                     address: u64,
                     size: u32,
                     data: &mut [u8]|
                     -> Result<(), UnknownError> {
                        warn!(
                            "rsc read pc {:x?} write {address:x} data {data:x?}",
                            proc.cpu.pc()
                        );
                        let offset = address - 0xeb20_0000;
                        // DRV_PRNT_CHLD_CONFIG
                        //
                        // Number of TCS has to be greater than 0xe
                        if offset == 0xc {
                            info!("access DRV_PRNT_CHLD_CONFIG");
                            data.copy_from_slice(&((0x11u32 << 27) | 0xfu32).to_le_bytes());
                        } else if offset == 0x1000c {
                            info!("access DRV_PRNT_CHLD_CONFIG");
                            data.copy_from_slice(&((0x11u32 << 27) | 0x5u32).to_le_bytes());
                        } else if offset == 0xfb8 {
                            data.copy_from_slice(&1u32.to_le_bytes());
                        }
                        Ok(())
                    },
                ),
            ),
            // rsc peripheral (??? no)
            StyxHook::MemoryReadVirtual(
                (0xa2163020..0xa2163030).into(),
                Box::new(
                    |proc: CoreHandle,
                     address: u64,
                     size: u32,
                     data: &mut [u8]|
                     -> Result<(), UnknownError> {
                        if address == 0xa2163020 {
                            data.copy_from_slice(&1u32.to_le_bytes());
                        } else if address == 0xa216302c {
                            data.copy_from_slice(&1u32.to_le_bytes());
                        }
                        Ok(())
                    },
                ),
            ),
            StyxHook::MemoryRead(
                (0xe001030..(0xe001030 + 0xb0)).into(),
                Box::new(
                    |proc: CoreHandle,
                     address: u64,
                     size: u32,
                     data: &mut [u8]|
                     -> Result<(), UnknownError> {
                        warn!(
                            "chipinfo memory write pc {:x?} {address:x} size {size} data {data:?}",
                            proc.cpu.pc()
                        );
                        Ok(())
                    },
                ),
            ),
            // pdc related
            // should write stuff to 0xc
            StyxHook::MemoryReadVirtual(
                (0xa42e1000..0xa42ee000).into(),
                Box::new(
                    |proc: CoreHandle,
                     address: u64,
                     size: u32,
                     data: &mut [u8]|
                     -> Result<(), UnknownError> {
                        if address == 0xa42e1004 {
                            data.copy_from_slice(&0x54c0u32.to_le_bytes());
                        } else if address == 0xa42e1008 {
                            data.copy_from_slice(&0x30_0000u32.to_le_bytes());
                        } else {
                            info!(
                                "unknown pdc access size {size} address {address:x} data {data:x?} pc {:x?}", proc.cpu.pc()
                            );
                        }
                        Ok(())
                    },
                ),
            ),
            // Something related to waipio chipset/revision/whatever. Firmware needs this.
            /*StyxHook::MemoryRead(
                (0x1fc8000..0x1fc8004).into(),
                Box::new(
                    |proc: CoreHandle, address: u64, size: u32, data: &mut [u8]| {
                        data.copy_from_slice(&0xa001_0000_u32.to_le_bytes());

                        Ok(())
                    },
                ),
            ),*/
        ])
    }

    fn post_init(&self, proc: &mut Processor) -> Result<(), UnknownError> {
        let memory = proc.memory().data();
        // Mystery peripheral
        memory.write(0x10c2004).le().value(1u32).unwrap();
        memory.write(0x10c2000).le().value(1u32).unwrap();
        const SMEM_BASE_ADDR: u32 = 0xe00_0000;
        const SMEM_SIZE: u32 = 0x100000;

        // Something related to waipio chipset/revision/whatever. Firmware needs this.
        memory.write(0x1fc8000).le().value(0xa001_0000u32)?;

        /*memory.write(0x1fd4000).le().value(SMEM_BASE_ADDR)?;
        // ???
        memory.write(0x1fd4004).le().value(0xababababu32)?;

        // see struct smem_addr_info and smem_get_base_addr
        // SMEM_TARGET_INFO_IDENTIFIER
        // according to QEMU this is SMEM_ADDR
        memory.write(0xe00_0000).le().value(0x49494953u32)?;
        // size
        memory.write(0xe00_0004).le().value(SMEM_SIZE)?;
        // phy_addr
        memory.write(0xe00_0008).le().value(SMEM_BASE_ADDR)?;

        /*for i in 0..(0x10000 / 4) {
            memory.write(0xe00_2000 + i as u64).le().value(i as u32)?;
        }*/

        // bloop
        memory.write(SMEM_BASE_ADDR + 0xc0).le().value(1u32)?;
        for i in 0..32 {
            memory
                .write(SMEM_BASE_ADDR + 0x42 + (i * 4))
                .le()
                .value(12u16)?;
        }
        // memory.write(0xe00_205c as u64).le().value(1u32)?;

        const SMEM_TOC: u32 = SMEM_BASE_ADDR + SMEM_SIZE - 0x1000;
        const SMEM_PARTHEADER_OFF: u32 = 0x1000;
        const SMEM_PARTHEADER: u32 = SMEM_BASE_ADDR + SMEM_PARTHEADER_OFF;
        // magic $TOC
        memory.write(SMEM_TOC).le().value(0x434f5424u32)?;
        // "SMEM_READ_SMEM_4(&toc->version) == 1"
        memory.write(SMEM_TOC + 4).le().value(1u32)?;
        // canot be greater than 18, but should be zero to facilitate initialization
        // toc number of entries
        memory.write(SMEM_TOC + 8).le().value(1)?;

        // partition table address (offset)
        memory
            .write(SMEM_TOC + 0x20)
            .le()
            .value(SMEM_PARTHEADER_OFF)?;
        // partition table size
        memory.write(SMEM_TOC + 0x24).le().value(0x10000u32)?;
        //
        memory.write(SMEM_TOC + 0x28).le().value(0u32)?;
        // host0
        memory.write(SMEM_TOC + 0x2c).le().value(0xfffeu16)?;
        // host1
        memory.write(SMEM_TOC + 0x2e).le().value(0xfffeu16)?;

        // part table
        memory.write(SMEM_PARTHEADER).le().value(0x54525024)?;
        // host0 (global host)
        memory.write(SMEM_PARTHEADER + 0x4).le().value(0xfffeu16)?;
        // host1 (global host)
        memory.write(SMEM_PARTHEADER + 0x6).le().value(0xfffeu16)?;
        memory.write(SMEM_PARTHEADER + 0x8).le().value(0x10000u32)?;
        // this value might be wrong
        memory.write(SMEM_PARTHEADER + 0xc).le().value(0xa0u32)?;
        memory
            .write(SMEM_PARTHEADER + 0x10)
            .le()
            .value(0x10000u32)?;

        // private entry
        memory.write(SMEM_PARTHEADER + 0x20).le().value(0xa5a5u16)?;
        // id
        memory.write(SMEM_PARTHEADER + 0x22).le().value(0x89u16)?;
        memory.write(SMEM_PARTHEADER + 0x24).le().value(0xb0u32)?;
        for i in 0..0xb0 {
            memory
                .write(SMEM_PARTHEADER + 0x30 + i)
                .le()
                .value(0xa8u8)?;
        }
        memory.write(SMEM_PARTHEADER + 0x30).le().value(0u32)?;*/

        Ok(())
    }
}

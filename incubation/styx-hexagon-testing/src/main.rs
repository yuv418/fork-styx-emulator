// SPDX-License-Identifier: BSD-2-Clause
use styx_emulator::cpu::arch::hexagon::{
    gdb_targets::HexagonHvxCpuTargetDescription, HexagonRegister,
};
use styx_emulator::prelude::gdb::{GDBOptions, GdbExecutor, GdbPluginParams, StepIRQs};
use styx_emulator::prelude::log::{info, trace, warn};
use styx_emulator::prelude::logging::init_logging;
use styx_emulator::prelude::*;
use styx_emulator::processors::hexagon::hexagon::HexagonBuilder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();

    let shld_debug = std::env::var("HEX_LLDB").is_ok_and(|_| true);
    let test_case = std::env::var("TEST_CASE").is_ok_and(|_| true);
    let p5 = std::env::var("PIXEL5").is_ok_and(|_| true);

    let mut proc = ProcessorBuilder::default()
        .with_builder(HexagonBuilder::default())
        .with_backend(Backend::Pcode)
        .with_loader(ElfLoader::default());

    if shld_debug {
        let gdb_params = GdbPluginParams::tcp("0.0.0.0", 9999, true);
        proc = proc.with_executor(
            GdbExecutor::<HexagonHvxCpuTargetDescription>::new(gdb_params)?.with_options(
                GDBOptions {
                    step_irqs: StepIRQs::Enabled,
                    ..Default::default()
                },
            ),
        )
    }

    let mut proc = proc
        .with_target_program(std::env::args().nth(1).unwrap())
        .build()?;

    // setup cfgbase
    //
    // FW reads from cfgbase, uses that addr to read phys mem
    // then takes the read addr and padds some address and then reads
    //
    //
    //
    /*
    INFO styx_cpu_pcode_backend::arch_spec::hexagon::system::mem: memw_phys reading from 8, rs 8 rt 0
    INFO styx_cpu_pcode_backend::arch_spec::hexagon::system::mem: memw_phys read 100
    INFO styx_cpu_pcode_backend::arch_spec::hexagon::system::mem: memw_phys reading from f80044, rs 44 rt 1f00
    INFO styx_cpu_pcode_backend::arch_spec::hexagon::system::mem: memw_phys read 0
    WARN styx_cpu_pcode_backend::call_other: Handle index 44 (name: Some("syncht")) does not exist. Called with: [] -> Some(Unique(0x5BBF00, 4)) @ 0x8cc00494
    INFO styx_cpu_pcode_backend::arch_spec::hexagon::system::mem: memw_phys reading from c, rs c rt 0
    INFO styx_cpu_pcode_backend::arch_spec::hexagon::system::mem: memw_phys read 0
    INFO styx_cpu_pcode_backend::arch_spec::hexagon::system::mem: memw_phys reading from 10, rs 10 rt 0
    INFO styx_cpu_pcode_backend::arch_spec::hexagon::system::mem: memw_phys read 0
       */

    // we then read from, (((X << 5) - 0x100) << 11) + struct offset
    // subsystem base
    /*proc.core.cpu.add_hook(StyxHook::MemoryReadVirtual(
        (0xFE11db04..0xFE11db14).into(),
        Box::new(
            |mut proc: CoreHandle, address: u64, size: u32, data: &mut [u8]| {
                let lr = proc.cpu.read_register::<u32>(HexagonRegister::Lr).unwrap();
                unimplemented!(
                    "memory read hook at {:x?}, address {address:x} lr {lr:x}",
                    proc.pc()
                )
            },
        ),
    ))?;*/

    /*proc.core.cpu.add_hook(StyxHook::RegisterWrite(
        HexagonRegister::BadVa0.into(),
        Box::new(|proc: CoreHandle, reg, data: &RegisterValue| {
            warn!("badva0 written pc {:x?}", proc.cpu.pc());
            Ok(())
        }),
    ))?;*/

    proc.core.cpu.add_hook(StyxHook::MemoryReadVirtual(
        (0x9db6c000..0x9db6c004).into(),
        Box::new(
            |mut proc: CoreHandle, address: u64, size: u32, data: &mut [u8]| {
                warn!(
                    "l2ecomem written {:x?}, address is {address:x} value is {data:x?} sz {size}",
                    proc.pc()
                );
                Ok(())
            },
        ),
    ))?;

    proc.core.cpu.add_hook(StyxHook::MemoryRead(
        (0xd8200000..0xd8210000).into(),
        Box::new(
            |mut proc: CoreHandle, address: u64, size: u32, data: &mut [u8]| {
                warn!(
                    "l2ecomem read {:x?}, address is {address:x} value is {data:?} sz {size}",
                    proc.pc()
                );
                Ok(())
            },
        ),
    ))?;

    proc.core.cpu.add_hook(StyxHook::MemoryWriteVirtual(
        (0xfe1bfd40..0xfe1bfd48).into(),
        Box::new(
            |mut proc: CoreHandle, address: u64, size: u32, data: &[u8]| {
                let lr = proc.cpu.read_register::<u32>(HexagonRegister::Lr).unwrap();
                warn!(
                    "memory write hook at {:x?}, address is {address:x} lr {lr:x} value is {data:?} sz {size}",
                    proc.pc()
                );
                Ok(())
            },
        ),
    ))?;
    proc.core.cpu.add_hook(StyxHook::MemoryReadVirtual(
        (0xfe1bfd40..0xfe1bfd48).into(),
        Box::new(
            |mut proc: CoreHandle, address: u64, size: u32, data: &mut [u8]| {
                let lr = proc.cpu.read_register::<u32>(HexagonRegister::Lr).unwrap();
                warn!(
                    "memory read hook at {:x?}, address is {address:x} lr {lr:x} value is {data:?} sz {size}",
                    proc.pc()
                );
                Ok(())
            },
        ),
    ))?;

    proc.core.cpu.add_hook(StyxHook::MemoryWriteVirtual(
        (0xfe1bfc40..0xfe1bfc48).into(),
        Box::new(
            |mut proc: CoreHandle, address: u64, size: u32, data: &[u8]| {
                let lr = proc.cpu.read_register::<u32>(HexagonRegister::Lr).unwrap();
                warn!(
                    "memory write hook at {:x?}, address is {address:x} lr {lr:x} value is {data:?} sz {size}",
                    proc.pc()
                );
                Ok(())
            },
        ),
    ))?;
    // proc.core.mmu.write_u32_le_phys_data(0xf80044, 0x100)?;

    proc.core
        .cpu
        .write_register(HexagonRegister::CfgBase, 0x0000d838_u32)
        .unwrap();

    // l2tcm base - also experimentally determined.
    proc.core.mmu.write_u32_le_phys_data(0xd8380000, 0x540)?;

    // l2ecomem size
    proc.core.mmu.write_u32_le_phys_data(0xd8380044, 0x800)?;

    // subsystem base
    proc.core.mmu.write_u32_le_phys_data(0xd8380008, 0xfc90)?;

    // l2cfg_base
    proc.core.mmu.write_u32_le_phys_data(0xd8380010, 0x57a)?;
    // etm base
    proc.core.mmu.write_u32_le_phys_data(0xd838000c, 0x579)?;
    // fastl2vic base
    proc.core.mmu.write_u32_le_phys_data(0xd8380028, 0x57e)?;
    // l2itcm base
    proc.core.mmu.write_u32_le_phys_data(0xd838005c, 0x560)?;

    // what are these??? reserved2... QURTK_clade_cfg_base
    // this is experimentally determined
    proc.core.mmu.write_u32_le_phys_data(0xd8380024, 0x57d)?;
    // reserved3 QURTK_clade2_cfg_bsae
    proc.core.mmu.write_u32_le_phys_data(0xd8380060, 0x55b)?;

    // the test case tries to set up the ISDB, which
    // we are not implementing.
    //
    // according to qemu "hw/hexagon/hexagon_dsp.c"
    // the isdb_secure flag is at 0x30 anjd isdb_trusted is
    // 0x34. we will set these to true to avoid the isdb being used
    // or something.
    //
    // isdb = in-silicon debugger
    if test_case {
        // isdb_secure
        proc.core.mmu.write_u32_le_phys_data(0x30, 1)?;
        // isdb_trusted
        proc.core.mmu.write_u32_le_phys_data(0x34, 1)?;

        // used by test suite: number of tlb entries
        // TODO: use the constant
        proc.core.mmu.write_u32_le_phys_data(0xd838002c, 0xc0)?;

        // i honestly have no idea what this is
        // something related to l2 cache?
        proc.core.mmu.write_u32_le_phys_data(0xd8380040, 0x800)?;
    }

    // This instruction takes far too long, so we must fixing.
    /*
    bc499670  00674014   { p0 = cmp.eq(r0,r7); if (!p0.new) jump:t 0xbc499670;
    bc499674  004400b0     r0 = add(r0,#32);
    bc499678  00c0c0a0     dczeroa(r0); }
    */

    proc.core.cpu.add_hook(StyxHook::CodeVirtual(
        0xbc499670u64.into(),
        Box::new(|proc: CoreHandle| {
            trace!("playing with pc for long insn");
            // update r0 to equal r7, since this
            // packet breaks its loop after r7 is equal to r0
            let r7 = proc.cpu.read_register::<u32>(HexagonRegister::R7).unwrap();
            proc.cpu.write_register(HexagonRegister::R0, r7).unwrap();

            proc.cpu
                .write_register(HexagonRegister::Pc, 0xbc49967cu32)
                .unwrap();
            Ok(())
        }),
    ))?;
    // idk
    if p5 {
        info!("writing pixel 5");
        proc.core.mmu.write_u32_le_phys_data(0x04122000, 0x100)?;
        proc.core
            .mmu
            .write_u32_le_phys_data(0x4090000, 0xffffffff)?;

        proc.core.cpu.add_hook(StyxHook::MemoryRead(
            (0x04080028..0x0408002c).into(),
            Box::new(
                |proc: CoreHandle, _address: u64, _size: u32, _data: &mut [u8]| {
                    proc.mmu.write_u32_le_phys_data(0x04080028, 0xfffffff0)?;

                    Ok(())
                },
            ),
        ))?;

        proc.core.cpu.add_hook(StyxHook::MemoryRead(
            (0x04122000..0x04122004).into(),
            Box::new(
                |proc: CoreHandle, _address: u64, _size: u32, _data: &mut [u8]| {
                    let val = proc.mmu.read_u32_le_phys_data(0x04122000)?;
                    proc.mmu.write_u32_le_phys_data(0x04122000, val + 0x100)?;

                    Ok(())
                },
            ),
        ))?;

        proc.core.cpu.add_hook(StyxHook::CodeVirtual(
            0xc021ca90u64.into(),
            Box::new(|proc: CoreHandle| {
                trace!("playing with pc for long insn");
                // update r0 to equal r7, since this
                // packet breaks its loop after r7 is equal to r0
                let r7 = proc.cpu.read_register::<u32>(HexagonRegister::R7).unwrap();
                proc.cpu.write_register(HexagonRegister::R0, r7).unwrap();

                proc.cpu
                    .write_register(HexagonRegister::Pc, 0xc021ca9cu32)
                    .unwrap();
                Ok(())
            }),
        ))?;
    }

    info!("Starting emulator");

    proc.run(Forever)?;

    Ok(())
}

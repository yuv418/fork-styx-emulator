// SPDX-License-Identifier: BSD-2-Clause

use smallvec::SmallVec;
use styx_core::{
    arch::hexagon::{register_fields::Syscfg, GlobalHexagonRegister, HexagonRegister},
    core::VCpuCore,
    cpu::{CpuBackendExt, HexagonInterruptType, HexagonLockType},
    event_controller::{ActivateIRQnError, InterruptExecuted},
    macrolib::debug,
    prelude::{
        log::{debug, error, info, trace, warn},
        Context, ExceptionNumber,
    },
};

/// QEMU target/hexagon/op_helper.c I think.
#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum HexagonLockState {
    Unlocked,
    LockHeld,
    Queued,
    Waiting,
}

pub fn start(
    vcpu_idx: usize,
    irq: ExceptionNumber,
    value: u64,
    vcpus: &mut [VCpuCore],
) -> Result<InterruptExecuted, ActivateIRQnError> {
    let thread_mask = value as u32;
    info!("start handler with {thread_mask:x}");

    let sz = 32;

    for t in 0..sz {
        // Check thread for mask
        if thread_mask & (1 << t) != 0 {
            info!("starting thread {t} pc {:x?}", vcpus[vcpu_idx].cpu.pc());
            // Don't start a thread that doesn't exist
            if t < vcpus.len() {
                vcpus[t]
                    .event_controller
                    .execute(
                        HexagonInterruptType::ThreadStart as ExceptionNumber,
                        vcpus[t].cpu.as_mut(),
                        &mut vcpus[t].mmu,
                    )
                    .with_context(|| "couldn't send event to cpu to start thread")?;
            } else {
                warn!("don't start thread {t} that doesn't exist");
            }
        }
    }

    Ok(InterruptExecuted::Executed)
}

pub fn lock(
    vcpu_idx: usize,
    irq: ExceptionNumber,
    value: u64,
    vcpus: &mut [VCpuCore],
    lock_type: HexagonLockType,
    lock_state: &mut SmallVec<[HexagonLockState; 16]>,
) -> Result<InterruptExecuted, ActivateIRQnError> {
    let mut cpu = &mut vcpus[vcpu_idx].cpu;
    let mut mmu = &mut vcpus[vcpu_idx].mmu;

    let syscfg = Syscfg::new_with_raw_value(
        cpu.read_register::<u32>(GlobalHexagonRegister::SysCfg)
            .with_context(|| "couldn't read Syscfg register")?,
    );

    let lock = match lock_type {
        HexagonLockType::K0 => syscfg.k0lock(),
        HexagonLockType::Tlb => syscfg.tlblock(),
    };

    let htid = cpu
        .read_register::<u32>(HexagonRegister::Htid)
        .with_context(|| "couldn't read htid register")?;

    let lock_state_this_thread = { lock_state[htid as usize] };

    let mut give_lock = || -> Result<InterruptExecuted, ActivateIRQnError> {
        // The lock is acquired
        cpu.write_register(
            GlobalHexagonRegister::SysCfg,
            match lock_type {
                HexagonLockType::K0 => syscfg.with_k0lock(true),
                HexagonLockType::Tlb => syscfg.with_tlblock(true),
            }
            .raw_value(),
        )
        .with_context(|| "couldn't write syscfg")?;

        // This is bizarre and crazy
        let pc = cpu.pc().with_context(|| "couldn't get pc")?;
        cpu.set_pc(pc + 4)
            .with_context(|| "couldn't advance tlblock pc")?;

        lock_state[htid as usize] = HexagonLockState::LockHeld;
        Ok(InterruptExecuted::Executed)
    };

    // Already locked... according to hexagon QEMU in target/hexagon/op_helper.c,
    // must halt if double locked.
    //
    // TODO: queuing when multithread. When the thread is unlocked, we take the thread to unlock
    // and set its lock status to queued. Then the waiting thread is awoken.
    if lock {
        match lock_state_this_thread {
            HexagonLockState::LockHeld => {
                // Halting interrupt. According to qemu, the same thread that holds the lock locking itself
                // would be a double interrupt. (deadlocks?)
                warn!("htid {htid} tried to lock twice");

                cpu.handle_event(&mut mmu, HexagonInterruptType::LockSleep as ExceptionNumber)?;
                Ok(InterruptExecuted::Executed)
            }
            // We need to queue the lock.
            HexagonLockState::Unlocked => {
                lock_state[htid as usize] = HexagonLockState::Waiting;

                info!("lock held but {htid} wants it, halting {htid} till lock released");

                cpu.handle_event(&mut mmu, HexagonInterruptType::LockSleep as ExceptionNumber)?;
                Ok(InterruptExecuted::Executed)
            }
            // If we are running the instruction with this, then we
            // need to give it the lock now.
            HexagonLockState::Queued => {
                info!("lock was held but {htid} was queued, giving lock to thread {htid}");
                give_lock()
            }
            _ => unreachable!(),
        }
    }
    // No one else
    else {
        info!("lock not held by anyone, giving lock to thread {htid}");
        give_lock()
    }
}

pub fn unlock(
    vcpu_idx: usize,
    irq: ExceptionNumber,
    value: u64,
    vcpus: &mut [VCpuCore],
    lock_type: HexagonLockType,
    lock_state: &mut SmallVec<[HexagonLockState; 16]>,
) -> Result<InterruptExecuted, ActivateIRQnError> {
    let mut cpu = &mut vcpus[vcpu_idx].cpu;
    let mut event_controller = &mut vcpus[vcpu_idx].event_controller;
    let mut mmu = &mut vcpus[vcpu_idx].mmu;

    let syscfg = Syscfg::new_with_raw_value(
        cpu.read_register::<u32>(GlobalHexagonRegister::SysCfg)
            .with_context(|| "couldn't read Syscfg register")?,
    );

    let lock = match lock_type {
        HexagonLockType::K0 => syscfg.k0lock(),
        HexagonLockType::Tlb => syscfg.tlblock(),
    };
    let htid = cpu
        .read_register::<u32>(HexagonRegister::Htid)
        .with_context(|| "couldn't read htid register")?;

    // Never had the lock
    if !lock {
        // Halting interrupt. According to qemu, this would be a double interrupt.
        warn!("{:?} never locked", lock_type);

        Ok(InterruptExecuted::Executed)
    }
    // According to hexagon QEMU (hex-next quic qemu) in target/hexagon/op_helper.c,
    // we must use a round-robin method of choosing the next thread to get the lock.
    // Since we are single-threaded, we can just unlock.
    else {
        if lock_state[htid as usize] != HexagonLockState::LockHeld {
            warn!("htid {htid} tried to unlock a lock that is not held by them!!");

            Ok(InterruptExecuted::Executed)
        } else {
            // The lock is released
            cpu.write_register(
                GlobalHexagonRegister::SysCfg,
                match lock_type {
                    HexagonLockType::K0 => syscfg.with_k0lock(false),
                    HexagonLockType::Tlb => syscfg.with_tlblock(false),
                }
                .raw_value(),
            )
            .with_context(|| "couldn't write syscfg")?;

            lock_state[htid as usize] = HexagonLockState::Unlocked;

            // Now figure out if anyone else is getting the lock.
            for i in 0..lock_state.len() {
                // Adjust
                let i = (htid as usize + i) % lock_state.len();

                if let HexagonLockState::Waiting = lock_state[i] {
                    // Give them the lock
                    info!("in tlbunlock, giving htid {i} the lock");
                    let syscfg = Syscfg::new_with_raw_value(
                        cpu.read_register::<u32>(GlobalHexagonRegister::SysCfg)
                            .with_context(|| "couldn't read syscfg")?,
                    );

                    cpu.write_register(
                        GlobalHexagonRegister::SysCfg,
                        match lock_type {
                            HexagonLockType::K0 => syscfg.with_k0lock(false),
                            HexagonLockType::Tlb => syscfg.with_tlblock(false),
                        }
                        .raw_value(),
                    )
                    .with_context(|| "couldn't write syscfg")?;

                    lock_state[i] = HexagonLockState::Queued;

                    vcpus[i]
                        .event_controller
                        .execute(
                            HexagonInterruptType::LockWake as ExceptionNumber,
                            vcpus[i].cpu.as_mut(),
                            &mut vcpus[i].mmu,
                        )
                        .with_context(|| "couldn't execute unlock to other vcpu")?;

                    break;
                    // ipend check that QEMU does
                }
            }

            Ok(InterruptExecuted::Executed)
        }
    }
}

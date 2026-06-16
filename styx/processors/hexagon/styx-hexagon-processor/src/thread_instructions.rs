// SPDX-License-Identifier: BSD-2-Clause

use arbitrary_int::{u9, Number};
use smallvec::SmallVec;
use styx_core::{
    arch::hexagon::{
        register_fields::{Bestwait, ModeCtl, SchedCfg, Ssr, Stid, Syscfg},
        GlobalHexagonRegister, HexagonRegister,
    },
    core::VcpuCore,
    cpu::{CpuBackendExt, HexagonInterruptCause, HexagonInterruptType, HexagonLockType},
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
    vcpus: &mut [VcpuCore],
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

pub fn nmi(
    vcpu_idx: usize,
    irq: ExceptionNumber,
    value: u64,
    vcpus: &mut [VcpuCore],
) -> Result<InterruptExecuted, ActivateIRQnError> {
    let thread_mask = value as u32;
    info!("nmi handler with {thread_mask:x}");

    let sz = 32;

    for t in 0..sz {
        // Check thread for mask
        if thread_mask & (1 << t) != 0 {
            info!("nmi to thread {t} pc {:x?}", vcpus[t].cpu.pc());
            // Don't start a thread that doesn't exist
            if t < vcpus.len() {
                let ssr = Ssr::new_with_raw_value(
                    vcpus[t]
                        .cpu
                        .read_register::<u32>(HexagonRegister::Ssr)
                        .with_context(|| "couldn't read ssr register")?,
                );
                vcpus[t]
                    .cpu
                    .write_register(
                        HexagonRegister::Ssr,
                        ssr.with_cause(HexagonInterruptCause::ImpreciseNmi as u8)
                            .raw_value(),
                    )
                    .with_context(|| "couldn't read ssr register")?;

                vcpus[t]
                    .event_controller
                    .execute(
                        HexagonInterruptType::Imprecise as ExceptionNumber,
                        vcpus[t].cpu.as_mut(),
                        &mut vcpus[t].mmu,
                    )
                    .with_context(|| "couldn't send event to cpu to start thread")?;
            } else {
                warn!("don't nmi to thread {t} that doesn't exist");
            }
        }
    }

    Ok(InterruptExecuted::Executed)
}

pub fn lock(
    vcpu_idx: usize,
    irq: ExceptionNumber,
    value: u64,
    vcpus: &mut [VcpuCore],
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

    info!(
        "in lock, lock state is {lock_state:x?} and pc is {:x?} and idx is {vcpu_idx}",
        cpu.pc()
    );
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
        info!("give_lock, lock state is {lock_state:x?}");

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
                warn!(
                    "htid {htid} tried to lock twice lock_state {lock_state:x?} at pc {:x?}",
                    cpu.pc()
                );

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
        info!("lock not held by anyone, giving lock to thread {htid}",);
        give_lock()
    }
}

pub fn unlock(
    vcpu_idx: usize,
    irq: ExceptionNumber,
    value: u64,
    vcpus: &mut [VcpuCore],
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

    info!("in unlock, lock state is {lock_state:x?}");

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
            lock_state[htid as usize] = HexagonLockState::Unlocked;

            // Only unlock in SYSCFG if the queued is not true
            let mut queued = false;

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

                    lock_state[i] = HexagonLockState::Queued;

                    vcpus[i]
                        .event_controller
                        .execute(
                            HexagonInterruptType::LockWake as ExceptionNumber,
                            vcpus[i].cpu.as_mut(),
                            &mut vcpus[i].mmu,
                        )
                        .with_context(|| "couldn't execute unlock to other vcpu")?;

                    queued = true;
                    break;
                    // ipend check that QEMU does
                }
            }

            if !queued {
                info!("releasing syscfg lock");
                vcpus[vcpu_idx]
                    .cpu
                    .write_register(
                        GlobalHexagonRegister::SysCfg,
                        match lock_type {
                            HexagonLockType::K0 => syscfg.with_k0lock(false),
                            HexagonLockType::Tlb => syscfg.with_tlblock(false),
                        }
                        .raw_value(),
                    )
                    .with_context(|| "couldn't write syscfg")?;
            }

            Ok(InterruptExecuted::Executed)
        }
    }
}

pub fn resched(
    vcpu_idx: usize,
    irq: ExceptionNumber,
    value: u64,
    vcpus: &mut [VcpuCore],
) -> Result<InterruptExecuted, ActivateIRQnError> {
    // Figure out whether the current "bestwait" is higher priority than the lowest priority
    // currently running thread. The lowest priority thread is the one with the highest STID.PRIO
    // number.

    // (TID, PRIO)
    info!("running resched");

    // First check if we are even enabled.
    let schedcfg = SchedCfg::new_with_raw_value(
        vcpus[vcpu_idx]
            .cpu
            .read_register::<u32>(GlobalHexagonRegister::SchedCfg)
            .with_context(|| "couldn't read schedcfg")?,
    );

    if !schedcfg.en() {
        info!("resched not enabled");
        return Ok(InterruptExecuted::Executed);
    }

    let mut currently_running_lowest_prio: (i32, u8) = (-1, 0);

    let modectl = ModeCtl::new_with_raw_value(
        vcpus[vcpu_idx]
            .cpu
            .read_register::<u32>(GlobalHexagonRegister::ModeCtl)
            .with_context(|| "couldn't read modectl")?,
    );
    info!("modectl is {:?}", modectl);

    for (i, core) in vcpus.iter_mut().enumerate() {
        // Check if the thread is actually enabled. Don't bother with the thread if
        // the thread is not enabled.
        if (modectl.enable_mask() & (1 << i as u16)) == 0 {
            info!("skipping resched check for thread {i} since it is not enabled");
            continue;
        }

        let stid = Stid::new_with_raw_value(
            core.cpu
                .read_register::<u32>(HexagonRegister::Stid)
                .with_context(|| "couldn't read stid")?,
        );
        let prio = stid.prio();

        // This prio is lower priority than currently running lowest.
        if prio > currently_running_lowest_prio.1 {
            info!("prio {prio} crlp {}", currently_running_lowest_prio.1);
            currently_running_lowest_prio.0 = i as i32;
            currently_running_lowest_prio.1 = prio;
        }
    }

    // Now get bestwait priority.
    let bestwait = Bestwait::new_with_raw_value(
        vcpus[vcpu_idx]
            .cpu
            .read_register::<u32>(GlobalHexagonRegister::BestWait)
            .with_context(|| "couldn't read bestwait")?,
    );

    info!(
        "bestwait {:x} lowest_prio {:x?}",
        bestwait.raw_value(),
        currently_running_lowest_prio
    );

    if currently_running_lowest_prio.0 != -1
        && currently_running_lowest_prio.1 > bestwait.prio().value() as u8
    {
        // Time to preempt.
        let preempt_thread = currently_running_lowest_prio.0 as usize;
        info!("resched is preempting thread {preempt_thread}");

        // New priority written here
        vcpus[preempt_thread]
            .cpu
            .write_register(
                GlobalHexagonRegister::BestWait,
                bestwait.with_prio(u9::MAX).raw_value(),
            )
            .with_context(|| "couldn't write bestwait")?;
        // TODO: do I need to set stid.prio?

        let ssr = Ssr::new_with_raw_value(
            vcpus[vcpu_idx]
                .cpu
                .read_register::<u32>(HexagonRegister::Ssr)
                .with_context(|| "couldn't read ssr")?,
        );

        vcpus[preempt_thread]
            .cpu
            .write_register(
                HexagonRegister::Ssr,
                ssr.with_cause(HexagonInterruptCause::Int0 as u8 + schedcfg.intno().value() as u8)
                    .raw_value(),
            )
            .with_context(|| "couldn't write bestwait")?;
        vcpus[preempt_thread]
            .event_controller
            .execute(
                HexagonInterruptType::Int0 as i32 + schedcfg.intno().value() as i32,
                vcpus[preempt_thread].cpu.as_mut(),
                &mut vcpus[preempt_thread].mmu,
            )
            .with_context(|| "couldn't execute resched event")?;
    } else {
        info!("resched is not happening right now");
    }

    Ok(InterruptExecuted::Executed)
}

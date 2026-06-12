// SPDX-License-Identifier: BSD-2-Clause
//! Shared logic for tlb/k0lock instructions.
//! See 11.9.2 "acquire hardware lock"

use std::{str::FromStr, sync::Mutex};

use log::{debug, info, warn};
use smallvec::smallvec;
use smallvec::SmallVec;
use styx_cpu_type::arch::hexagon::{
    register_fields::Syscfg, GlobalHexagonRegister, HexagonRegister,
};
use styx_cpu_type::TargetExitReason;
use styx_errors::anyhow::Context;
use styx_pcode::{pcode::VarnodeData, sla::SlaUserOps};
use styx_pcode_translator::sla::HexagonUserOps;
use styx_processor::event_controller::ExceptionNumber;
use styx_processor::{
    cpu::{CpuBackend, CpuBackendExt},
    event_controller::EventController,
    memory::Mmu,
};
use styx_sync::lazy_static;

use crate::{
    arch_spec::ArchSpecBuilder,
    call_other::{CallOtherCallback, CallOtherCpu, CallOtherHandleError},
    HexagonPcodeBackend, PCodeStateChange,
};

use super::interrupt::HexagonInterruptType;

#[derive(Debug)]
pub enum HexagonLockType {
    K0,
    Tlb,
}

#[derive(Debug)]
pub struct HardwareLock {
    lock_type: HexagonLockType,
}
impl<T: CpuBackend> CallOtherCallback<T> for HardwareLock {
    fn handle(
        &mut self,
        cpu: &mut dyn CallOtherCpu<T>,
        mmu: &mut Mmu,
        ev: &mut EventController,
        _inputs: &[VarnodeData],
        _output: Option<&VarnodeData>,
    ) -> Result<PCodeStateChange, CallOtherHandleError> {
        ev.execute_primary(
            match self.lock_type {
                HexagonLockType::K0 => HexagonInterruptType::K0lockInstruction,
                HexagonLockType::Tlb => HexagonInterruptType::TlblockInstruction,
            } as i32,
            0,
        )
        .with_context(|| "couldn't execute instruction to primary EC")?;
        Ok(PCodeStateChange::ExitRerun(
            TargetExitReason::InstructionCountComplete,
        ))
    }
}

#[derive(Debug)]
pub struct HardwareUnlock {
    lock_type: HexagonLockType,
}
impl<T: CpuBackend> CallOtherCallback<T> for HardwareUnlock {
    fn handle(
        &mut self,
        cpu: &mut dyn CallOtherCpu<T>,
        _mmu: &mut Mmu,
        ev: &mut EventController,
        _inputs: &[VarnodeData],
        _output: Option<&VarnodeData>,
    ) -> Result<PCodeStateChange, CallOtherHandleError> {
        ev.execute_primary(
            match self.lock_type {
                HexagonLockType::K0 => HexagonInterruptType::K0UnlockInstruction,
                HexagonLockType::Tlb => HexagonInterruptType::TlbUnlockInstruction,
            } as i32,
            0,
        )
        .with_context(|| "couldn't execute instruction to primary EC")?;
        Ok(PCodeStateChange::Exit(
            TargetExitReason::InstructionCountComplete,
        ))
    }
}

pub fn add_lock_callothers<S: SlaUserOps<UserOps: FromStr>>(
    spec: &mut ArchSpecBuilder<S, HexagonPcodeBackend>,
) {
    spec.call_other_manager
        .add_handler_other_sla(
            HexagonUserOps::Tlblock,
            HardwareLock {
                lock_type: HexagonLockType::Tlb,
            },
        )
        .unwrap();

    spec.call_other_manager
        .add_handler_other_sla(
            HexagonUserOps::K0lock,
            HardwareLock {
                lock_type: HexagonLockType::K0,
            },
        )
        .unwrap();

    spec.call_other_manager
        .add_handler_other_sla(
            HexagonUserOps::Tlbunlock,
            HardwareUnlock {
                lock_type: HexagonLockType::Tlb,
            },
        )
        .unwrap();

    spec.call_other_manager
        .add_handler_other_sla(
            HexagonUserOps::K0unlock,
            HardwareUnlock {
                lock_type: HexagonLockType::K0,
            },
        )
        .unwrap();
}

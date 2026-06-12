// SPDX-License-Identifier: BSD-2-Clause

use std::str::FromStr;

use log::info;
use styx_cpu_type::{arch::hexagon::HexagonRegister, TargetExitReason};
use styx_errors::anyhow::Context;
use styx_pcode::{pcode::VarnodeData, sla::SlaUserOps};
use styx_pcode_translator::sla::HexagonUserOps;
use styx_processor::{
    cpu::{CpuBackend, CpuBackendExt},
    event_controller::{EventController, ExceptionNumber},
    memory::Mmu,
};

use crate::{
    arch_spec::ArchSpecBuilder,
    call_other::{CallOtherCallback, CallOtherCpu, CallOtherHandleError},
    HexagonInterruptType, HexagonPcodeBackend, PCodeStateChange,
};

/// Handle the Hexagon wait instruction. Stubbed for now.
/// See 11.9.2 SYSTEM MONITOR, "Transition threads to Wait mode" for documentation.
/// Also see QUIC QEMU, target/hexagon/imported/system.idef, Y2_wait.
#[derive(Debug)]
pub struct WaitHandler {}

impl<T: CpuBackend> CallOtherCallback<T> for WaitHandler {
    fn handle(
        &mut self,
        _cpu: &mut dyn CallOtherCpu<T>,
        _mmu: &mut Mmu,
        _ev: &mut EventController,
        _inputs: &[VarnodeData],
        _output: Option<&VarnodeData>,
    ) -> Result<PCodeStateChange, CallOtherHandleError> {
        Ok(PCodeStateChange::Fallthrough)
    }
}

#[derive(Debug)]
pub struct StartHandler {}

impl<T: CpuBackend> CallOtherCallback<T> for StartHandler {
    fn handle(
        &mut self,
        cpu: &mut dyn CallOtherCpu<T>,
        _mmu: &mut Mmu,
        ev: &mut EventController,
        inputs: &[VarnodeData],
        _output: Option<&VarnodeData>,
    ) -> Result<PCodeStateChange, CallOtherHandleError> {
        let thread_mask_vn = &inputs[0];
        let thread_mask = cpu
            .read(thread_mask_vn)
            .with_context(|| "couldn't get thread mask for thread mask")?
            .to_u64()
            .with_context(|| "couldn't convert thread mas to u64")?;

        info!("start handler with {thread_mask:x}");

        // need to make it so that this thread doesn't just switch to a different one with this.
        ev.execute_primary(
            HexagonInterruptType::StartInstruction as i32,
            thread_mask as u64,
        )
        .with_context(|| "couldn't execute instruction to primary EC")?;

        Ok(PCodeStateChange::Exit(
            TargetExitReason::InstructionCountComplete,
        ))
    }
}

#[derive(Debug)]
pub struct StopHandler {}

impl<T: CpuBackend> CallOtherCallback<T> for StopHandler {
    fn handle(
        &mut self,
        cpu: &mut dyn CallOtherCpu<T>,
        mmu: &mut Mmu,
        _ev: &mut EventController,
        _inputs: &[VarnodeData],
        _output: Option<&VarnodeData>,
    ) -> Result<PCodeStateChange, CallOtherHandleError> {
        let htid = cpu
            .read_register::<u32>(HexagonRegister::Htid)
            .with_context(|| "couldn't get htid in stop")?;
        info!("stopping thread {htid}");
        cpu.handle_event(mmu, HexagonInterruptType::ThreadStop as ExceptionNumber)?;

        Ok(PCodeStateChange::Exit(
            TargetExitReason::InstructionCountComplete,
        ))
    }
}

pub fn add_thread_callothers<S: SlaUserOps<UserOps: FromStr>>(
    spec: &mut ArchSpecBuilder<S, HexagonPcodeBackend>,
) {
    spec.call_other_manager
        .add_handler_other_sla(HexagonUserOps::Wait, WaitHandler {})
        .unwrap();
    spec.call_other_manager
        .add_handler_other_sla(HexagonUserOps::Start, StartHandler {})
        .unwrap();
    spec.call_other_manager
        .add_handler_other_sla(HexagonUserOps::Stop, StopHandler {})
        .unwrap();
}

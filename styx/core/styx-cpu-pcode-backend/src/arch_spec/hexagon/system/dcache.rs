// SPDX-License-Identifier: BSD-2-Clause
use derive_more::FromStr;
use log::{debug, info};
use styx_errors::{anyhow::Context, UnknownError};
use styx_pcode::{pcode::VarnodeData, sla::SlaUserOps};
use styx_pcode_translator::sla::HexagonUserOps;
use styx_processor::{
    cpu::CpuBackend,
    event_controller::EventController,
    memory::{Mmu, MmuOpError},
};

use crate::{
    arch_spec::{ArchSpecBuilder, HexagonPcodeBackend},
    call_other::{CallOtherCallback, CallOtherCpu, CallOtherHandleError},
    PCodeStateChange,
};

// Data cache

#[derive(Debug)]
pub struct DcacheGenericStub {
    from: &'static str,
}

impl<T: CpuBackend> CallOtherCallback<T> for DcacheGenericStub {
    fn handle(
        &mut self,
        _backend: &mut dyn CallOtherCpu<T>,
        _mmu: &mut Mmu,
        _ev: &mut EventController,
        _inputs: &[VarnodeData],
        _output: Option<&VarnodeData>,
    ) -> Result<PCodeStateChange, CallOtherHandleError> {
        debug!("dcache stub called for {}", self.from);
        Ok(PCodeStateChange::Fallthrough)
    }
}

#[derive(Debug)]
pub struct Dczeroa;
impl<T: CpuBackend> CallOtherCallback<T> for Dczeroa {
    fn handle(
        &mut self,
        backend: &mut dyn CallOtherCpu<T>,
        mmu: &mut Mmu,
        _ev: &mut EventController,
        inputs: &[VarnodeData],
        _output: Option<&VarnodeData>,
    ) -> Result<PCodeStateChange, CallOtherHandleError> {
        let addr = backend
            .read(&inputs[0])
            .with_context(|| "couldn't read dczeroa")?
            .to_u64()
            .with_context(|| "couldn't convert addr to u64")?;
        info!("addr {:x}", addr);

        // There are really only two places where we have to care about page faults
        // in CallOthers. This is one of them. Eventually, we should keep it DRY
        // and put the exception handler into execute_pcode.rs.
        match mmu.virt_write_data(addr, &[0; 32], backend) {
            Ok(_) => Ok(PCodeStateChange::Fallthrough),
            Err(MmuOpError::TlbException(irq)) => Ok(PCodeStateChange::Exception(irq)),
            Err(e) => Err(UnknownError::context(
                e.into(),
                "couldn't call virt_write_data in dczeroa",
            )
            .into()),
        }
    }
}

pub fn add_dcache_callothers<S: SlaUserOps<UserOps: FromStr>>(
    spec: &mut ArchSpecBuilder<S, HexagonPcodeBackend>,
) {
    spec.call_other_manager
        .add_handler_other_sla(HexagonUserOps::Dctagr, DcacheGenericStub { from: "dctagr" })
        .unwrap();

    spec.call_other_manager
        .add_handler_other_sla(HexagonUserOps::Dctagw, DcacheGenericStub { from: "dctagw" })
        .unwrap();

    spec.call_other_manager
        .add_handler_other_sla(
            HexagonUserOps::Dcfetch,
            DcacheGenericStub { from: "dcfetch" },
        )
        .unwrap();

    spec.call_other_manager
        .add_handler_other_sla(HexagonUserOps::Dckill, DcacheGenericStub { from: "dckill" })
        .unwrap();

    spec.call_other_manager
        .add_handler_other_sla(HexagonUserOps::Dczeroa, Dczeroa)
        .unwrap();

    spec.call_other_manager
        .add_handler_other_sla(
            HexagonUserOps::Dccleana,
            DcacheGenericStub { from: "dccleana" },
        )
        .unwrap();

    spec.call_other_manager
        .add_handler_other_sla(
            HexagonUserOps::Dccleanidx,
            DcacheGenericStub { from: "dccleanidx" },
        )
        .unwrap();

    spec.call_other_manager
        .add_handler_other_sla(
            HexagonUserOps::Dccleaninva,
            DcacheGenericStub {
                from: "dccleaninva",
            },
        )
        .unwrap();
    spec.call_other_manager
        .add_handler_other_sla(
            HexagonUserOps::Dccleaninvidx,
            DcacheGenericStub {
                from: "dccleaninvidx",
            },
        )
        .unwrap();

    spec.call_other_manager
        .add_handler_other_sla(HexagonUserOps::Dcinva, DcacheGenericStub { from: "dcinva" })
        .unwrap();

    spec.call_other_manager
        .add_handler_other_sla(
            HexagonUserOps::Dcinvidx,
            DcacheGenericStub { from: "dcinvidx" },
        )
        .unwrap();
}

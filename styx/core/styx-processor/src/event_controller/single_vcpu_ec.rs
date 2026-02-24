// SPDX-License-Identifier: BSD-2-Clause

use super::*;

/// Primary event controller that routes interrupts to vcpu 0.
#[derive(Default)]
pub struct SingleVcpuEventController {}

impl EventDistributorImpl for SingleVcpuEventController {
    fn tick(
        &mut self,
        _delta: &GlobalDelta,
        pending_irqs: &[ExceptionNumber],
        vcpus: &mut [VcpuCore],
    ) -> Result<(), UnknownError> {
        if let Some(vcpu) = vcpus.first_mut() {
            for irq in pending_irqs {
                vcpu.event_controller.latch(*irq)?;
            }
        }
        Ok(())
    }
}

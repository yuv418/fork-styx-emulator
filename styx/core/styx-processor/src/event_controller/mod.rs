// SPDX-License-Identifier: BSD-2-Clause
//! Part of the Processor Core used for managing peripherals and handling interrupts.
mod dummy;
mod peripheral;
mod peripherals;
mod single_vcpu_ec;

use std::borrow::Cow;
use std::fmt::Display;
use std::{any::type_name, sync::Arc};

use as_any::AsAny;
pub use dummy::{DummyEventController, DummyEventDistributor};
use log::{info, trace};
pub use peripheral::{DummyPeripheral, Peripheral, PeripheralTickCtx, RaisedIrqs};
pub use peripherals::Peripherals;
pub use single_vcpu_ec::SingleVcpuEventController;
use smallvec::{Drain, SmallVec};
use static_assertions::assert_obj_safe;
use styx_errors::anyhow::Context;
use styx_errors::UnknownError;
use thiserror::Error;

use crate::core::VcpuId;
use crate::{
    core::VcpuCore,
    cpu::CpuBackend,
    executor::{time::GlobalDelta, Delta},
    memory::{MemoryBackend, Mmu},
    processor::Config,
};

pub type ExceptionNumber = i32;

#[derive(Debug)]
pub enum InterruptExecuted {
    Executed,
    NotExecuted,
}

#[derive(thiserror::Error, Debug)]
pub enum ActivateIRQnError {
    #[error("invalid Event `{0:?}` for this controller")]
    InvalidIRQn(ExceptionNumber),
    #[error(transparent)]
    Unknown(#[from] UnknownError),
}

assert_obj_safe!(EventControllerImpl);

/// Per-vCPU interrupt controller interface.
///
/// Handles the interrupt lifecycle for a single virtual CPU: queuing, executing,
/// and completing interrupts. Does not own peripherals which are managed by
/// the [`EventDistributor`].
pub trait EventControllerImpl: AsAny + Send {
    /// retrieve and execute the highest priority interrupt
    fn next(
        &mut self,
        cpu: &mut dyn CpuBackend,
        mmu: &mut Mmu,
    ) -> Result<InterruptExecuted, UnknownError>;

    /// queue an interrupt to be executed
    fn latch(&mut self, event: ExceptionNumber) -> Result<(), ActivateIRQnError>;

    /// directly execute an interrupt (useful for things like syscall)
    fn execute(
        &mut self,
        irq: ExceptionNumber,
        cpu: &mut dyn CpuBackend,
        mmu: &mut Mmu,
    ) -> Result<InterruptExecuted, ActivateIRQnError>;

    fn on_processor_start(
        &mut self,
        _cpu: &mut dyn CpuBackend,
        _mmu: &mut Mmu,
    ) -> Result<(), UnknownError> {
        Ok(())
    }

    fn on_processor_stop(
        &mut self,
        _cpu: &mut dyn CpuBackend,
        _mmu: &mut Mmu,
    ) -> Result<(), UnknownError> {
        Ok(())
    }

    /// Update state of the event controller.
    fn tick(
        &mut self,
        _cpu: &mut dyn CpuBackend,
        _mmu: &mut Mmu,
        _delta: &Delta,
    ) -> Result<(), UnknownError> {
        Ok(())
    }

    fn finish_interrupt(
        &mut self,
        cpu: &mut dyn CpuBackend,
        mmu: &mut Mmu,
    ) -> Option<ExceptionNumber>;

    fn init(
        &mut self,
        cpu: &mut dyn CpuBackend,
        mmu: &mut MemoryBackend,
        config: &mut Config,
    ) -> Result<(), UnknownError>;

    fn reset(&mut self, _cpu: &mut dyn CpuBackend, _mmu: &mut Mmu) -> Result<(), UnknownError> {
        Ok(())
    }

    /// What is the current running exception?
    ///
    /// - `Ok(None)` indicates no exception is running.
    /// - `Err(CurrentExceptionError::Unsupported)` indicates this feature is not available on this
    ///   event controller.
    fn current_exception(&mut self) -> Result<Option<Exception>, OptionalFeatureError> {
        Err(OptionalFeatureError::Unsupported)
    }

    fn available_exceptions(&mut self) -> Result<Cow<'_, [Exception]>, OptionalFeatureError> {
        Err(OptionalFeatureError::Unsupported)
    }
}

#[derive(Error, Debug)]
pub enum OptionalFeatureError {
    #[error(transparent)]
    Other(#[from] UnknownError),
    #[error("feature not supported by this event controller")]
    Unsupported,
}

/// Debug/Introspection representation of an Exception.
///
/// Returned by [`EventController::current_exception()`] to show the currently
/// running exception.
///
/// Used by gdbserver to report running exception via a monitor command.
#[derive(Clone)]
pub struct Exception {
    pub name: Cow<'static, str>,
    pub number: ExceptionNumber,
}

impl Display for Exception {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

assert_obj_safe!(EventDistributorImpl);

/// Processor-level interrupt controller interface.
///
/// Handles processor-wide lifecycle events and routes peripheral interrupts to
/// the appropriate vCPU. Peripheral ownership and dispatch is managed by
/// [`EventDistributor`].
pub trait EventDistributorImpl: AsAny + Send {
    fn on_processor_start(&mut self, _vcpus: &mut [VcpuCore]) -> Result<(), UnknownError> {
        Ok(())
    }

    fn on_processor_stop(&mut self, _vcpus: &mut [VcpuCore]) -> Result<(), UnknownError> {
        Ok(())
    }

    /// Update state of the event controller.
    ///
    /// Called once per emulation round after all peripherals have been ticked.
    /// `delta` has processor level timekeeping information that peripherals and
    /// the event distributor schedule against.
    ///
    /// `pending_irqs` contains exception numbers returned by peripheral ticks.
    /// Route them to the appropriate vCPU's event controller.
    /// The pending irqs are assumed to be handled after this tick.
    #[allow(unused_variables)]
    fn tick(
        &mut self,
        delta: &GlobalDelta,
        pending_irqs: &[ExceptionNumber],
        vcpus: &mut [VcpuCore],
    ) -> Result<(), UnknownError> {
        Ok(())
    }

    fn execute(
        &mut self,
        _vcpu_idx: usize,
        _value: u64,
        _irq: ExceptionNumber,
        _vcpus: &mut [VcpuCore],
    ) -> Result<InterruptExecuted, ActivateIRQnError> {
        Ok(InterruptExecuted::NotExecuted)
    }

    #[allow(unused_variables)]
    fn init(
        &mut self,
        vcpus: &mut [VcpuCore],
        memory: &Arc<MemoryBackend>,
        config: &mut Config,
    ) -> Result<(), UnknownError> {
        Ok(())
    }

    fn reset(&mut self, _cpu: &mut dyn CpuBackend, _mmu: &mut Mmu) -> Result<(), UnknownError> {
        Ok(())
    }
}

/// Wraps a [`EventControllerImpl`] and delegates all per-vCPU interrupt operations to it.
///
/// Does not own peripherals. Peripheral management is the responsibility of [`EventDistributor`].
///
/// The dummy implementation provides a dummy event controller on vcpu 0. Good for tests.
pub struct EventController {
    /// Inner, cpu specific implementation of the event controller.
    pub inner: Box<dyn EventControllerImpl>,
    /// Which vcpu does this event controller belong to.
    pub vcpu_index: VcpuId,
    /// IRQs to latch on other vcpus
    pub irqs_to: SmallVec<[(ExceptionNumber, u64); 4]>,
}

impl Default for EventController {
    fn default() -> Self {
        Self::new(Box::new(DummyEventController::default()), 0)
    }
}

impl EventController {
    pub fn new(inner: Box<dyn EventControllerImpl>, vcpu_index: VcpuId) -> Self {
        Self {
            inner,
            vcpu_index,
            irqs_to: Default::default(),
        }
    }

    /// Provides a dummy event controller on vcpu 0. Good for tests.
    pub fn dummy() -> Self {
        Self::new(Box::new(DummyEventController::default()), 0)
    }

    pub fn next(
        &mut self,
        cpu: &mut dyn CpuBackend,
        mmu: &mut Mmu,
    ) -> Result<InterruptExecuted, UnknownError> {
        trace!("secondary event controller next");
        self.inner.next(cpu, mmu)
    }

    pub fn latch(&mut self, event: ExceptionNumber) -> Result<(), ActivateIRQnError> {
        self.inner.latch(event)
    }

    pub fn primary_irqs_contain(&self, event: ExceptionNumber) -> bool {
        for (ev, _) in self.irqs_to.iter() {
            if *ev == event {
                return true;
            }
        }

        return false;
    }

    /// This is used for when you want to execute an event on the primary event controller.
    /// Since the primary event controller can see every Vcpu, events that need shared state
    /// or information from all CPUs should be executed here.
    pub fn execute_primary(
        &mut self,
        event: ExceptionNumber,
        value: u64,
    ) -> Result<(), ActivateIRQnError> {
        self.irqs_to.push((event, value));
        info!("adding to IRQ, {:?}", self.irqs_to);
        Ok(())
    }

    pub fn vcpu_irqs(
        &mut self,
    ) -> Result<SmallVec<[(ExceptionNumber, u64); 4]>, ActivateIRQnError> {
        let irqs = self.irqs_to.clone();
        self.irqs_to.clear();

        Ok(irqs)
    }

    pub fn execute(
        &mut self,
        irq: ExceptionNumber,
        cpu: &mut dyn CpuBackend,
        mmu: &mut Mmu,
    ) -> Result<InterruptExecuted, ActivateIRQnError> {
        self.inner.execute(irq, cpu, mmu)
    }

    pub fn on_processor_start(
        &mut self,
        cpu: &mut dyn CpuBackend,
        mmu: &mut Mmu,
    ) -> Result<(), UnknownError> {
        trace!("secondary event controller processor_start");
        self.inner.on_processor_start(cpu, mmu)
    }

    pub fn on_processor_stop(
        &mut self,
        cpu: &mut dyn CpuBackend,
        mmu: &mut Mmu,
    ) -> Result<(), UnknownError> {
        trace!("secondary event controller processor_stop");
        self.inner.on_processor_stop(cpu, mmu)
    }

    pub fn tick(
        &mut self,
        cpu: &mut dyn CpuBackend,
        mmu: &mut Mmu,
        delta: &Delta,
    ) -> Result<(), UnknownError> {
        trace!("ticking secondary event controller");
        self.inner.tick(cpu, mmu, delta)
    }

    pub fn reset(&mut self, cpu: &mut dyn CpuBackend, mmu: &mut Mmu) -> Result<(), UnknownError> {
        self.inner.reset(cpu, mmu)
    }

    /// What is the current running exception?
    ///
    /// Intended for debugging and introspection (e.g. the gdbserver `event info` monitor
    /// command); this is not used to drive interrupt dispatch. For controllers that support
    /// preemption or nested exceptions, implementations return the top of the active
    /// exception stack.
    ///
    /// - `Ok(None)` indicates no exception is running.
    /// - `Err(OptionalFeatureError::Unsupported)` indicates this feature is not available on this
    ///   event controller.
    pub fn current_exception(&mut self) -> Result<Option<Exception>, OptionalFeatureError> {
        self.inner.current_exception()
    }

    pub fn get_impl<T: EventControllerImpl + 'static>(&mut self) -> Result<&mut T, UnknownError> {
        self.inner
            .as_mut()
            .as_any_mut()
            .downcast_mut()
            .with_context(|| {
                format!(
                    "could not downcast secondary event controller impl to {:?}",
                    type_name::<T>()
                )
            })
    }
}

/// The event distributor owns all peripherals attached to a processor and handles
/// processor-level lifecycle events.
pub struct EventDistributor {
    /// Processor-level event controller implementation.
    pub inner: Box<dyn EventDistributorImpl>,
    pub peripherals: Peripherals,
    /// IRQs to latch on other vcpus
    pub irqs_to: SmallVec<[(usize, ExceptionNumber); 4]>,
}

impl Default for EventDistributor {
    fn default() -> Self {
        Self::new(Box::new(DummyEventDistributor::default()))
    }
}

impl EventDistributor {
    pub fn new(inner: Box<dyn EventDistributorImpl>) -> Self {
        Self {
            inner,
            peripherals: Peripherals::default(),
            irqs_to: Default::default(),
        }
    }

    pub fn on_processor_start(&mut self, vcpus: &mut [VcpuCore]) -> Result<(), UnknownError> {
        trace!("processor_start event distributor");
        self.inner.on_processor_start(vcpus)?;
        for peripheral in self.peripherals.peripherals.iter_mut() {
            peripheral.on_processor_start(vcpus, self.inner.as_mut())?;
        }
        Ok(())
    }

    pub fn on_processor_stop(&mut self, vcpus: &mut [VcpuCore]) -> Result<(), UnknownError> {
        trace!("processor_stop event distributor");
        self.inner.on_processor_stop(vcpus)?;
        for peripheral in self.peripherals.peripherals.iter_mut() {
            peripheral.on_processor_stop(vcpus, self.inner.as_mut())?;
        }
        Ok(())
    }

    pub fn reset(&mut self, cpu: &mut dyn CpuBackend, mmu: &mut Mmu) -> Result<(), UnknownError> {
        self.inner.reset(cpu, mmu)?;
        for peripheral in self.peripherals.peripherals.iter_mut() {
            peripheral.reset(cpu, mmu)?;
        }
        Ok(())
    }

    pub fn add_peripheral(&mut self, peripheral: Box<dyn Peripheral>) -> Result<(), UnknownError> {
        self.peripherals.insert_peripheral(peripheral)
    }

    /// Tick all peripherals and route their interrupts.
    ///
    /// Called once per emulation round (after all vCPUs have strided).
    /// Builds a [`PeripheralTickCtx`] (sharing the physical memory backend
    /// from `vcpus[0]`, since all vCPUs share the same `Arc<MemoryBackend>`),
    /// iterates peripherals, collects returned IRQs, then delegates
    /// routing to the inner [`EventDistributorImpl`].
    ///
    /// If `vcpus` is empty, peripherals are not ticked this round. The
    /// inner routing call is still made with an empty `pending_irqs`
    /// slice, preserving previous behavior.
    pub fn tick(
        &mut self,
        delta: &GlobalDelta,
        vcpus: &mut [VcpuCore],
    ) -> Result<(), UnknownError> {
        let mut pending_irqs = SmallVec::<[ExceptionNumber; 16]>::new();

        // Scope the immutable borrow of `vcpus` so it ends before we hand
        // `vcpus` mutably to `self.inner.tick` below.
        if let Some(vcpu) = vcpus.first() {
            let memory: &MemoryBackend = &vcpu.mmu.memory;
            let ctx = PeripheralTickCtx::new(delta, memory);
            for peripheral in &mut self.peripherals.peripherals {
                let raised = peripheral.tick(&ctx)?;
                pending_irqs.extend(raised);
            }
        }

        self.inner.tick(delta, &pending_irqs, vcpus)
    }

    /// Run an interrupt synchronously.
    /// Used for handling instructions and whatnot.
    pub fn execute(
        &mut self,
        vcpu_idx: usize,
        value: u64,
        irq: ExceptionNumber,
        vcpus: &mut [VcpuCore],
    ) -> Result<InterruptExecuted, ActivateIRQnError> {
        self.inner.execute(vcpu_idx, value, irq, vcpus)
    }

    pub fn get_impl<T: EventDistributorImpl + 'static>(&mut self) -> Result<&mut T, UnknownError> {
        self.inner
            .as_mut()
            .as_any_mut()
            .downcast_mut()
            .with_context(|| {
                format!(
                    "could not downcast event distributor impl to {:?}",
                    type_name::<T>()
                )
            })
    }
}

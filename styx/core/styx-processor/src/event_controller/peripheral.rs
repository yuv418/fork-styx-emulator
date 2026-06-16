// SPDX-License-Identifier: BSD-2-Clause
use as_any::AsAny;
use log::debug;
use smallvec::SmallVec;
use static_assertions::assert_obj_safe;
use styx_cpu_type::arch::backends::GlobalArchRegister;
use styx_cpu_type::arch::RegisterValueCompatible;
use styx_errors::UnknownError;

use super::{EventDistributorImpl, ExceptionNumber};
use crate::core::VcpuCore;
use crate::cpu::{CpuBackend, CpuBackendExt, ReadRegisterError, WriteRegisterError};
use crate::executor::time::GlobalDelta;
use crate::memory::{MemoryBackend, Mmu};
use crate::processor::BuildingProcessor;

/// Context passed to [`Peripheral::tick`] each emulation round.
///
/// Intentionally excludes any per-vCPU state: peripherals must not reach
/// into CPU registers or per-vCPU MMUs during tick. Physical memory is
/// fair game (via the shared [`MemoryBackend`]); anything CPU-adjacent
/// goes through hooks registered in [`Peripheral::init`].
/// I.e. for MMIO, use memory hooks.
///
/// Marked `#[non_exhaustive]` because the struct is expected to gain new
/// fields over time (e.g. a shared peripheral clock).
/// Construct via [`PeripheralTickCtx::new`].
#[non_exhaustive]
pub struct PeripheralTickCtx<'a> {
    vcpu: &'a mut dyn CpuBackend,
    pub delta: &'a GlobalDelta,
    pub memory: &'a MemoryBackend,
}

impl<'a> PeripheralTickCtx<'a> {
    pub fn new(
        vcpu: &'a mut dyn CpuBackend,
        delta: &'a GlobalDelta,
        memory: &'a MemoryBackend,
    ) -> Self {
        Self {
            vcpu,
            delta,
            memory,
        }
    }

    pub fn read_globalreg<V: RegisterValueCompatible>(
        &mut self,
        register: impl Into<GlobalArchRegister>,
    ) -> Result<V::ReturnValue, ReadRegisterError> {
        self.vcpu.read_register::<V>(register.into())
    }

    #[allow(unused)]
    pub fn write_globalreg(
        &mut self,
        reg: impl Into<GlobalArchRegister>,
        value: impl RegisterValueCompatible,
    ) -> Result<(), WriteRegisterError> {
        self.vcpu.write_register(reg.into(), value)
    }
}

/// The exceptions a peripheral has asked to be raised during a
/// [`Peripheral::tick`] call.
///
/// Built up and returned by the peripheral.
/// The [`EventDistributorImpl`] then routes each raised exception to the
/// appropriate vCPU.
///
/// Most peripherals raise zero or one exception per tick, so the backing
/// storage is stack-allocated (four inline slots) and heap-allocates only
/// for the degenerate case of a peripheral asserting many interrupts at
/// once.
///
/// # Examples
///
/// No interrupts raised:
///
/// ```
/// use styx_processor::event_controller::RaisedIrqs;
///
/// let raised = RaisedIrqs::none();
/// assert!(raised.is_empty());
/// ```
///
/// A single interrupt raised:
///
/// ```
/// use styx_processor::event_controller::RaisedIrqs;
///
/// let raised = RaisedIrqs::one(42);
/// assert_eq!(raised.as_slice(), &[42]);
/// ```
///
/// Multiple interrupts raised, built up with [`push`](RaisedIrqs::push):
///
/// ```
/// use styx_processor::event_controller::RaisedIrqs;
///
/// let mut raised = RaisedIrqs::none();
/// raised.push(3);
/// raised.push(7);
/// raised.push(11);
/// assert_eq!(raised.as_slice(), &[3, 7, 11]);
/// ```
///
/// Collecting from an iterator via [`FromIterator`]:
///
/// ```
/// use styx_processor::event_controller::RaisedIrqs;
///
/// let raised: RaisedIrqs = [1, 2, 3].into_iter().collect();
/// assert_eq!(raised.as_slice(), &[1, 2, 3]);
/// ```
///
/// Converting a single exception number via [`From<ExceptionNumber>`](From):
///
/// ```
/// use styx_processor::event_controller::RaisedIrqs;
///
/// let raised: RaisedIrqs = 42.into();
/// assert_eq!(raised.as_slice(), &[42]);
/// ```
#[derive(Debug, Clone, Default)]
pub struct RaisedIrqs {
    /// SmallVec used here to avoid a heap allocation
    /// but user api should not care.
    inner: SmallVec<[ExceptionNumber; 4]>,
}

impl RaisedIrqs {
    /// No interrupts raised.
    ///
    /// # Examples
    ///
    /// ```
    /// use styx_processor::event_controller::RaisedIrqs;
    ///
    /// let raised = RaisedIrqs::none();
    /// assert!(raised.is_empty());
    /// assert_eq!(raised.len(), 0);
    /// ```
    pub fn none() -> Self {
        Self::default()
    }

    /// A single interrupt raised.
    ///
    /// # Examples
    ///
    /// ```
    /// use styx_processor::event_controller::RaisedIrqs;
    ///
    /// let raised = RaisedIrqs::one(7);
    /// assert_eq!(raised.len(), 1);
    /// assert_eq!(raised.as_slice(), &[7]);
    /// ```
    pub fn one(irqn: ExceptionNumber) -> Self {
        let mut s = Self::none();
        s.inner.push(irqn);
        s
    }

    /// Add an interrupt to the set.
    ///
    /// # Examples
    ///
    /// ```
    /// use styx_processor::event_controller::RaisedIrqs;
    ///
    /// let mut raised = RaisedIrqs::none();
    /// raised.push(1);
    /// raised.push(2);
    /// assert_eq!(raised.as_slice(), &[1, 2]);
    /// ```
    pub fn push(&mut self, irqn: ExceptionNumber) {
        self.inner.push(irqn);
    }

    /// Are any interrupts raised?
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Number of interrupts raised.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Borrow as a slice (for draining / tests).
    pub fn as_slice(&self) -> &[ExceptionNumber] {
        self.inner.as_slice()
    }
}

impl From<ExceptionNumber> for RaisedIrqs {
    fn from(irqn: ExceptionNumber) -> Self {
        Self::one(irqn)
    }
}

impl FromIterator<ExceptionNumber> for RaisedIrqs {
    fn from_iter<I: IntoIterator<Item = ExceptionNumber>>(iter: I) -> Self {
        Self {
            inner: iter.into_iter().collect(),
        }
    }
}

impl Extend<ExceptionNumber> for RaisedIrqs {
    fn extend<I: IntoIterator<Item = ExceptionNumber>>(&mut self, iter: I) {
        self.inner.extend(iter);
    }
}

impl IntoIterator for RaisedIrqs {
    type Item = ExceptionNumber;
    type IntoIter = smallvec::IntoIter<[ExceptionNumber; 4]>;
    fn into_iter(self) -> Self::IntoIter {
        self.inner.into_iter()
    }
}

assert_obj_safe!(Peripheral);

/// The common interface for all Styx peripherals.
///
/// Implementing this trait gives peripheral implementations the ability
/// to register hooks with the processor, update state while the processor
/// is running, and to receive callbacks when the target software have
/// completed the ISR.
pub trait Peripheral: AsAny + Send {
    /// called before peripheral is added to the event controller
    fn init(&mut self, _proc: &mut BuildingProcessor) -> Result<(), UnknownError> {
        Ok(())
    }

    /// Reset the peripheral's state.
    fn reset(&mut self, _cpu: &mut dyn CpuBackend, _mmu: &mut Mmu) -> Result<(), UnknownError> {
        Ok(())
    }

    /// The name of the peripheral.
    ///
    /// This is used to check for duplicates, so peripheral names should be unique.
    fn name(&self) -> &str;

    /// Return the exception numbers that belong to this peripheral
    fn irqs(&self) -> Vec<ExceptionNumber> {
        vec![]
    }

    /// Called on processor start. Called each time the processor is started after pause.
    fn on_processor_start(
        &mut self,
        _vcpus: &mut [VcpuCore],
        _event_controller: &mut dyn EventDistributorImpl,
    ) -> Result<(), UnknownError> {
        Ok(())
    }

    /// Called on processor stop. Called each time the processor is pause.
    fn on_processor_stop(
        &mut self,
        _vcpus: &mut [VcpuCore],
        _event_controller: &mut dyn EventDistributorImpl,
    ) -> Result<(), UnknownError> {
        Ok(())
    }

    /// Called each emulation round to update peripheral state.
    ///
    /// Return the exceptions the peripheral wants raised; the
    /// [`EventDistributorImpl`] routes them to the appropriate vCPU.
    ///
    /// Peripheral ticks are for internal state updates plus (optionally)
    /// access to shared physical memory via `ctx.memory`. CPU register
    /// access is intentionally unavailable; use hooks registered during
    /// [`Peripheral::init()`] if you need per-vCPU CPU state.
    fn tick(&mut self, _ctx: &mut PeripheralTickCtx<'_>) -> Result<RaisedIrqs, UnknownError> {
        Ok(RaisedIrqs::none())
    }
}

#[derive(Default, Debug)]
/// A placeholder peripheral, does nothing.
pub struct DummyPeripheral;

impl Peripheral for DummyPeripheral {
    fn init(&mut self, _proc: &mut BuildingProcessor) -> Result<(), UnknownError> {
        debug!("dummy peripheral initialized");
        Ok(())
    }

    fn name(&self) -> &str {
        "DummyPeripheral"
    }
}

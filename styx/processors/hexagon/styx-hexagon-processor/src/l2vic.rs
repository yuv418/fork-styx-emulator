// SPDX-License-Identifier: BSD-2-Clause

//! Hexagon l2vic interrupt controller.
//! See quic qemu's docs/devel/hexagon-l2vic.rst, hw/intc/l2vic.c and include/hw/intc/l2vic.h
//!
//! And Arm PrimeCell PL190 interrupt controller documentation.
//!
//! It appears that the l2vic (QEMU) describes that
//! all interrupts go to VID 0, which is IRQ 2.

use std::sync::{Arc, Mutex};

use arbitrary_int::*;
use bitbybit::{bitenum, bitfield};
use smallvec::{smallvec, SmallVec};
use styx_core::arch::hexagon::GlobalHexagonRegister;
use styx_core::core::{VCpuCore, VcpuBundle};
use styx_core::cpu::{HexagonInterruptCause, HexagonLockType};
use styx_core::event_controller::{EventControllerImpl, EventDistributorImpl};
use styx_core::macros::peripheral_shared_state;
use styx_core::prelude::GlobalDelta;
use styx_core::processor::Config;
use styx_core::sync::styx_async::sync::broadcast;
use styx_core::{
    arch::hexagon::{
        register_fields::{Ipendad, Ssr},
        HexagonRegister,
    },
    cpu::{CpuBackend, CpuBackendExt, HexagonInterruptType},
    errors::UnknownError,
    event_controller::{ActivateIRQnError, InterruptExecuted, Peripherals},
    hooks::{CoreHandle, StyxHook},
    memory::{MemoryBackend, Mmu},
    prelude::{
        log::{error, info, trace, warn},
        Context, ExceptionNumber,
    },
};

use crate::shared_state_hooks::{peripheral_shared_state_read, peripheral_shared_state_write};
use crate::thread_instructions::{self, HexagonLockState};
use crate::write_cfgtable_field;
use crate::{angel, config::HexagonProcessorConfig};

const FASTL2VIC_CFGTABLE_OFFSET: u64 = 0x28;
const L2VIC_OFFSET: u64 = 0x10000;
const L2VIC_NUM_SLOTS: u64 = 32;
const L2VIC_CONFIG_START: u64 = 0x100;

/// The l2vic can handle 32 interrupts.
/// Each of these interrupts are configured
/// through the reigsters below. Each register
/// is 32 contiguous 4-byte values that pertains
/// to each interrupt. So you would have
/// 128 bytes of enable, then 128 bytes of
/// EnableClear, etc.
///
/// PL190 2.1 "there are 32 interrupt lines."
///
/// From include/hw/intc/l2vic.h in QUIC QEMU.
/// The commented offsets are the offsets
/// used in QEMU to indicate the start of a register
/// (including all 32 interrupts). The start of the block is
/// 0x100 after the l2vic base, but we encode the enum
/// as zero-offset for clarity.
///
/// NOTE these comments might not make a lot of sense.
#[derive(Debug)]
#[bitenum(u16, exhaustive = false)]
pub enum L2VicRegister {
    // 0x100
    Enable = 0,
    // 0x180
    EnableClear = 1,
    // 0x200
    EnableSet = 2,
    // 0x280
    Type = 3,
    // 0x300
    Unknown0 = 4,
    // 0x380
    Status = 5,
    // 0x400
    Clear = 6,
    // 0x480
    SoftInt = 7,
    // 0x500
    Pending = 8,
    // 0x580
    Unknown1 = 9,
    // 0x600
    GRP0 = 10,
    // 0x680
    GRP1 = 11,
    // 0x700
    GRP2 = 12,
    // 0x780
    GRP3 = 13,
}

/// See QEMU's hw/intc/l2vic.c, specifically
/// fastl2vic_write, for more details.
#[derive(Debug)]
#[bitenum(u2)]
pub enum FastL2VicCommand {
    Enable = 0,
    Disable = 1,
    // Goes to soft int according to fastl2vic_write in
    // hw/intc/l2vic.c
    Interrupt = 2,
}

/// See QEMU's hw/intc/l2vic.c, specifically
/// fastl2vic_write, for more details.
#[bitfield(u32, debug)]
pub struct FastL2VicControl {
    #[bits(0..=9, r)]
    irq: u10,
    #[bits(16..=17, r)]
    command: Option<FastL2VicCommand>,
}

impl FastL2VicControl {
    /// Each IRQ for the l2vic belongs to a specific slice. This function gets the
    /// slice that the IRQ in the command corresponds to.
    ///
    /// The IRQs are split such that IRQs 0-31 are in slice 0,
    /// 32 to 63 are in slice 1, 64 to 95 are in slice 3, etc..
    pub fn slice(&self) -> usize {
        self.irq().as_usize() / 32
    }

    /// Within the slice, what is the index of the IRQ?
    /// Eg. IRQ 66 would be index 2 within slice 2,
    /// as slice 2 contains IRQs 64 to 95, so then
    /// IRQ 64, slice 0
    /// IRQ 65, slice 1
    /// IRQ 66, slice 2
    ///
    /// Now, the parameter to an Enable/EnableSet/etc.
    /// is the index stored as the nth bit set
    /// within a 32-bit integer.
    /// IRQ 64, slice 0, bit 0b000
    /// IRQ 65, slice 1, bit 0b001
    /// IRQ 66, slice 2, bit 0b010
    /// IRQ 67, slice 3, bit 0b100
    /// etc.
    ///
    /// This function returns that bit value.
    pub fn irq_slice_bit(&self) -> u32 {
        1 << (self.irq().as_u32() % 32)
    }
}

/// Handle MMIO write to fastl2vic
/// register. See QEMU's hw/intc/l2vic.c
fn fastl2vic_mmio_write_hook(
    proc: CoreHandle,
    address: u64,
    size: u32,
    data: &[u8],
    l2vic: Arc<Mutex<L2VicSharedState>>,
) -> Result<(), UnknownError> {
    let mut l2vic = l2vic.lock().unwrap();

    let fastl2vic_control = FastL2VicControl::new_with_raw_value(u32::from_le_bytes(
        data.try_into()
            .with_context(|| "couldn't convert fastl2vic access into u32")?,
    ));

    error!(
        "fastl2vic access address {address:x} {size:x} {data:x?} pc {:x?}, register {:x?}",
        proc.cpu.pc(),
        fastl2vic_control
    );

    // See fastl2vic_write
    match fastl2vic_control.command() {
        // According to QEMU this is EnableSet
        Ok(FastL2VicCommand::Enable) => {
            l2vic.handle_register_write(
                L2VicRegister::EnableSet,
                fastl2vic_control.slice(),
                // As opposed to the IRQ
                fastl2vic_control.irq_slice_bit(),
            )?;
        }
        // According to QEMU this is EnableClear
        Ok(FastL2VicCommand::Disable) => {
            l2vic.handle_register_write(
                L2VicRegister::EnableClear,
                fastl2vic_control.slice(),
                fastl2vic_control.irq_slice_bit(),
            )?;
        }
        _ => unimplemented!(),
    }

    Ok(())
}

/// Handle MMIO read to l2vic's
/// registers. See QEMU's hw/intc/l2vic.c
fn l2vic_mmio_read_hook(
    proc: CoreHandle,
    address: u64,
    size: u32,
    data: &mut [u8],
    l2vic: Arc<Mutex<L2VicSharedState>>,
) -> Result<(), UnknownError> {
    let mut l2vic = l2vic.lock().unwrap();

    let (register, slot) = l2vic_get_register_slot(l2vic.l2vic_base(), address);

    info!(
         "l2vic base reading at 0x{address:x} and size 0x{size:x} with data {data:x?}, register {register:?} slot {slot} pc is {:x?}",
         proc.cpu.pc()
     );

    match register {
        Ok(L2VicRegister::Enable) => {
            // Enable register is 4 bytes
            assert_eq!(size, 4);
            data.copy_from_slice(&l2vic.slots[slot].enable.to_le_bytes());
            error!("l2vic read data is now {data:x?}");
        }
        Ok(L2VicRegister::Status) => {
            warn!("l2vic status read, unimplemented");
        }
        Ok(L2VicRegister::Pending) => {
            data.copy_from_slice(&l2vic.slots[slot].pending.to_le_bytes());
        }
        Ok(L2VicRegister::Type) => {
            data.copy_from_slice(&l2vic.slots[slot].int_type.to_le_bytes());
        }
        _ => todo!(),
    }

    Ok(())
}

fn l2vic_mmio_write_hook(
    proc: CoreHandle,
    address: u64,
    size: u32,
    data: &[u8],
    l2vic: Arc<Mutex<L2VicSharedState>>,
) -> Result<(), UnknownError> {
    let mut l2vic = l2vic.lock().unwrap();

    let (register, slot) = l2vic_get_register_slot(l2vic.l2vic_base(), address);

    // All the values are 32 bits, see QEMU l2vic.c and
    // L2VICState.
    let data_u32 = u32::from_le_bytes(
        data.try_into()
            .with_context(|| "couldn't get l2vic write as u32")?,
    );

    error!(
        "l2vic base writing at {address:x} and size {size:x} with data {data:x?}, pc is {:x?}",
        proc.cpu.pc()
    );

    match register {
        Ok(register) => {
            l2vic.handle_register_write(register, slot, data_u32)?;
        }
        Err(_) => todo!(),
    }

    Ok(())
}

fn l2vic_get_register_slot(l2vic_base: u64, address: u64) -> (Result<L2VicRegister, u16>, usize) {
    // Compute the register that we are writing to and the interrupt number
    let register = L2VicRegister::new_with_raw_value(
        ((address - l2vic_base - L2VIC_CONFIG_START) / (L2VIC_NUM_SLOTS * 4)) as u16,
    );

    // This finds the "offset" into the register array. Since each register value
    // is 4 bytes, we divide by four to get the actual interrupt number.
    let slot = (((address - l2vic_base - L2VIC_CONFIG_START) % (L2VIC_NUM_SLOTS * 4)) / 4) as usize;

    (register, slot)
}

#[peripheral_shared_state]
pub struct L2Vic {
    #[shared]
    slots: [L2VicSlot; L2VIC_NUM_SLOTS as usize],
    #[shared]
    vid: u32,
    /// Only initialized during init() when we have the processor config.
    #[shared]
    l2vic_base: Option<u64>,
    #[shared]
    vid_irq_base: Option<u64>,
    #[shared]
    fastl2vic_base: Option<u64>,
    k0lock_state: Mutex<SmallVec<[HexagonLockState; 16]>>,
    tlblock_state: Mutex<SmallVec<[HexagonLockState; 16]>>,
}

impl Default for L2Vic {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(L2VicSharedState {
                slots: Default::default(),
                vid: u32::MAX,

                // Only initialized during init() when we have the processor config.
                l2vic_base: None,
                vid_irq_base: None,
                fastl2vic_base: None,
            })),

            k0lock_state: Mutex::new(smallvec![HexagonLockState::Unlocked; 16]),
            tlblock_state: Mutex::new(smallvec![HexagonLockState::Unlocked; 16]),
        }
    }
}

/// QEMU mentions a lot about slices... what are these so-called slices?
/// In our case, it seems to just compute down to 32 interrupts.
#[derive(Default)]
pub struct L2VicSlot {
    // I think this is supposed to be 32 bits
    // where each bit represents the nth interrupt
    // for the nth slot.
    enable: u32,
    clear: u32,
    // 1 = edge triggered
    // 0 = level triggered
    // See fastint.c in qemu-hexagon-testing for an example.
    int_type: u32,
    // Is the IRQ being serviced at the moment?
    status: u32,
    pending: u32,
}

/// TODO change this to use a bitvector library
impl L2VicSlot {
    pub fn irqn_enable(&self, n: usize) -> bool {
        ((self.enable >> n) & 1) == 1
    }
    pub fn set_irqn_pending(&mut self, n: usize, value: bool) {
        if value {
            // Set bit
            self.pending |= 1 << n
        } else {
            // Clear bit
            self.pending &= !(1 << n)
        }
    }
    pub fn set_irqn_enable(&mut self, n: usize, value: bool) {
        if value {
            // Set bit
            self.enable |= 1 << n
        } else {
            // Clear bit
            self.enable &= !(1 << n)
        }
    }
    pub fn set_irqn_status(&mut self, n: usize, value: bool) {
        if value {
            // Set bit
            self.status |= 1 << n
        } else {
            // Clear bit
            self.status &= !(1 << n)
        }
    }
    pub fn irqn_pending(&self, n: usize) -> bool {
        ((self.pending >> n) & 1) == 1
    }

    pub fn irqn_int_type(&self, n: usize) -> bool {
        ((self.int_type >> n) & 1) == 1
    }
    pub fn irqn_status(&self, n: usize) -> bool {
        ((self.status >> n) & 1) == 1
    }
}

impl L2VicSharedState {
    fn vid_irq_base(&self) -> u64 {
        self.vid_irq_base
            .expect("couldn't get vid_irq_base base, expected to be set in init")
    }
    fn l2vic_base(&self) -> u64 {
        self.l2vic_base
            .expect("couldn't get l2vic base, expected to be set in init")
    }
    fn fastl2vic_base(&self) -> u64 {
        self.fastl2vic_base
            .expect("couldn't get fastl2vic base, expected to be set in init")
    }

    /// This will effectively set this IRQ to pending, and put it in the
    /// IRQ queue.
    ///
    /// This means when the next event are checked and executed,
    /// in `next`, this event an be considered.
    ///
    /// TODO Priority and use a queue to avoid performance issues
    fn latch(&mut self, event: ExceptionNumber) -> Result<(), ActivateIRQnError> {
        let slot = event as usize / L2VIC_NUM_SLOTS as usize;
        let slot_offset = (event as usize) % (L2VIC_NUM_SLOTS as usize);
        trace!("l2vic latch slot {slot} offset {slot_offset}");

        self.slots[slot].set_irqn_pending(slot_offset, true);
        Ok(())
    }

    fn handle_register_write(
        &mut self,
        register: L2VicRegister,
        slot: usize,
        data: u32,
    ) -> Result<(), UnknownError> {
        error!("register {register:?} slot {slot} data {data:x}");

        match register {
            L2VicRegister::Enable => {
                self.slots[slot].enable = data;
            }
            // l2vic_write in hw/intc/self.c:
            // is similar to Enable, but sets the bits in data in the
            // current enabled field instead of overwriting the enable field with the bits.
            L2VicRegister::EnableSet => {
                self.slots[slot].enable |= data;
                info!("EnableSet, new enable 0x{:x}", self.slots[slot].enable);
            }
            L2VicRegister::EnableClear => {
                info!(
                    "EnableClear, enable 0x{:x} data 0x{:x}",
                    self.slots[slot].enable, data
                );
                self.slots[slot].enable &= !data;
                info!("EnableClear, new enable 0x{:x} ", self.slots[slot].enable);
            }
            L2VicRegister::Type => {
                self.slots[slot].int_type = data;
            }
            L2VicRegister::Clear => {
                // wrong
                self.slots[slot].clear = data;
            }
            L2VicRegister::SoftInt => {
                // The irq number is the "first" bit set according
                // to QEMU's l2vic write
                let slot_offset = data.trailing_zeros();
                let irqn = (slot as u32 * L2VIC_NUM_SLOTS as u32) + slot_offset;
                info!("SoftInt irq {irqn}");

                // Cause an interrupt
                // QEMU l2vic_write: soft int is only accepted if the interrupt is enabled
                if self.slots[slot].irqn_int_type(slot_offset as usize) {
                    info!("doing SoftInt for irq {irqn}");
                    self.latch(irqn as i32)?;
                }
                // WARN: do I need to set INT_ENABLE here like qemu does?
            }
            _ => todo!(),
        }

        Ok(())
    }
}

impl L2Vic {
    /// See l2vic_update for more details
    /// in hw/intc/l2vic.c.
    /// TODO: need to use this for synchronous interrupts, don't do all this Vid logic stuff for that
    /// This logic should probably be moved to the PrimaryEventController.
    fn execute_async(
        &mut self,
        l2vic_irq_n: ExceptionNumber,
        vcpu: &mut VCpuCore,
    ) -> Result<(), ActivateIRQnError> {
        let mut l2vic = self.lock();

        // Don't execute if we are currently servicing something.
        // NOTE: this is slow.
        // TODO: optimize.
        // NOTE: does this mean the l2vic can only handle one interrupt a time?
        for slot in 0..(L2VIC_NUM_SLOTS as usize) {
            info!("slot slots {:x?}", l2vic.slots[slot].status);
            // Each L2Vic slot is 32 bits; means one slot has 32 IRQs
            for irq in 0..32 {
                if l2vic.slots[slot].irqn_status(irq) {
                    info!("execute_async not taken, IRQ being serviced.");
                    return Ok(());
                }
            }
        }

        // The VID is found by looking at interrupt groups
        // Each group has an IRQ encoded for each slot and some
        // other magic..
        //
        //
        // IRQ number -> interrupt groups. there are 4
        // Interrupt group is array each slots
        // get slot, and the VID is the last 3 bits of the irq
        // times four shifted right
        //
        // (irq mod 8) * 4
        // vid 2 bits
        //
        // slice mmmm mmmm mmmm mmmm mmmm mmmm mmmm mmmm
        //
        //
        // It seems like there is only one VID
        // as the "interrupt groups" are never written.
        let vid_n = 0;

        // Un-enable and un-pending the IRQ.

        // It looks like we only have one VID
        // in use since no other VIDs are setup in FW,
        // So we shall trigger the IRQ
        let irq = l2vic_irq_n as usize;
        let slot = &mut l2vic.slots[irq / L2VIC_NUM_SLOTS as usize];

        slot.set_irqn_status(irq, true);
        slot.set_irqn_enable(irq, false);
        slot.set_irqn_pending(irq, false);

        for slot in 0..(L2VIC_NUM_SLOTS as usize) {
            info!("post slot slots {:x?}", l2vic.slots[slot].status);
        }

        // The L2vic has four signals (VID0-3) to interrupt the CPU,
        // and each IRQ handled by the L2vic is signaled by
        // raising VID corresponding to the IRQ, according to QEMU
        // documentation.
        //
        // It looks like we only have one VID in use
        // since no other VIDs are setup in FW, so we shall trigger the IRQ
        // on the one VID.
        l2vic.vid = irq as u32;
        // Update the backing register store
        vcpu.cpu
            .write_register(GlobalHexagonRegister::Vid, l2vic.vid)
            .with_context(|| "couldn't update vid register")?;

        // We also need to set the SSR cause to the VID
        // See qemu_irq_pulse in hw/intc/l2vic.c in QUIC QEMU
        let irq_base_offset = l2vic.vid_irq_base() + vid_n;
        let ssr =
            Ssr::new_with_raw_value(vcpu.cpu.read_register::<u32>(HexagonRegister::Ssr).unwrap())
                .with_cause(HexagonInterruptCause::Int0 as u8 + irq_base_offset as u8);
        info!("ssr ssr {ssr:x?}");

        vcpu.cpu
            .write_register(HexagonRegister::Ssr, ssr.raw_value())
            .unwrap();

        // Actual irq number of the 16 irqs here.
        let hex_irq_number =
            HexagonInterruptType::Int0 as ExceptionNumber + irq_base_offset as ExceptionNumber;

        // Set the irq pending
        let mut ipendad = Ipendad::new_with_raw_value(
            vcpu.cpu
                .read_register::<u32>(GlobalHexagonRegister::Ipendad)
                .with_context(|| "couldn't read r0 in interrupt")?,
        );
        ipendad.set_ipend(ipendad.ipend() | (1 << hex_irq_number));
        vcpu.cpu
            .write_register(GlobalHexagonRegister::Ipendad, ipendad.raw_value())
            .with_context(|| "couldn't write ipendad")?;

        // NOTE: the interrupt number is dependent on the VID. Right now there is only
        // one VID used, but if VID groups are used in the future, the IRQ number would change.
        // 0x12 - VID0
        // 0x13 - VID1
        // 0x14 - VID2
        // etc.
        //
        // As the VID base offset is 0x2, we can add the base Int0 (0x10) to compute this.
        // TODO: should probably use the ipend reg write to trigger this.
        vcpu.event_controller.latch(hex_irq_number)?;
        Ok(())

        // todo!("l2vic execute")
    }
}

impl EventDistributorImpl for L2Vic {
    /// See l2vic_update_all for more details
    /// in hw/intc/l2vic.c.
    ///
    /// This function will retrieve and execute the latest interrupt
    /// based on which interrupts are currently pending.
    ///
    /// TODO: optimization as needed.
    fn tick(
        &mut self,
        delta: &GlobalDelta,
        pending_irqs: &[ExceptionNumber],
        vcpus: &mut [VCpuCore],
    ) -> Result<(), UnknownError> {
        trace!("l2vic tick called, vid is 0x{:x}", self.lock().vid);
        // Update the VID, since the register write hook won't work
        // from the CIAD instruction, so just take the "register" VID
        // and if they're not equal set to whatever it is.

        // Latch the pending IRQs from peripherals.
        for pending_irq in pending_irqs {
            self.latch(*pending_irq)?;
        }

        let selected_l2vic_irq = {
            let mut l2vic = self.lock();

            let vid = vcpus[0]
                .cpu
                .read_register::<u32>(GlobalHexagonRegister::Vid)
                .with_context(|| "Couldn't read VID register")?;

            // Ciad handling
            // WARN: this is how we "clear vid and re-enable l2vic operation."
            // The latter is supposed to only happen when ciad is written, but
            // it could also happen now when VID is written. It doesn't look like that ever happens,
            // but still.
            if l2vic.vid != vid {
                let clear_slot = (l2vic.vid / L2VIC_NUM_SLOTS as u32) as usize;
                let clear_off = (l2vic.vid % L2VIC_NUM_SLOTS as u32) as usize;

                info!("l2vic: vid was set in code or by ciad, new vid {vid:x} updating to 0x{vid:x} and clearing int {:x} (slot {:x} slot offset {:x}).", l2vic.vid, clear_slot, clear_off);

                // Only update if the VID was cleared to 0xffffffff
                if vid as i32 == -1 {
                    l2vic.slots[clear_slot].set_irqn_status(clear_off, false);
                }

                l2vic.vid = vid;
            }

            // We aren't going to do anything until
            // VID is cleared - see l2vic_updated in hw/intc/l2vic.c QEMU.
            //
            // VID seemingly holds the value of the current IRQ that was raised.
            //
            // As an example, msee qemu-hexagon-testing's "levelint.c" test.
            // Also see hexagon-l2vic.rst in QUIC QEMU
            //
            // WARN: The QUIC implementation seems to use the
            // first bit set to find if any int_statuses are enabled.
            //
            // Is this not equivalent?
            //
            // -1 is "invalid" vid
            if l2vic.vid != u32::MAX {
                // ------- TEST
                let mut int_statuses = false;
                for slot in &l2vic.slots {
                    if slot.status != 0 {
                        int_statuses = true;
                        break;
                    }
                }
                assert_eq!(int_statuses, l2vic.vid != 0);
                // --------- END TEST

                // Interrupt not executed.
                return Ok(());
            }

            // Now also fail if any sort of exception is being serviced (
            // eg. synchronous exceptions).
            let ssr = Ssr::new_with_raw_value(
                vcpus[0]
                    .cpu
                    .read_register::<u32>(HexagonRegister::Ssr)
                    .with_context(|| "couldn't read the Ssr register")?,
            );
            if ssr.ex() {
                trace!("interrupt not executed because SSR.EX = {}, maybe synchronous exception is happening", ssr.ex());

                // Interrupt not executed.
                return Ok(());
            }

            // Hexagon Programmer's Reference Manual:
            //
            // 11.9.2 SYSTEM MONITOR - "highest-priority interrupt 0... lowest-
            // priority interrupt 31"
            //
            // Go through IRQ numbers from lowest to highest priority,
            // and raise the highest-priority IRQ.

            let mut irq = None;
            for (slot_no, slot) in l2vic.slots.iter().enumerate() {
                // Now go through the IRQs
                // As each slot is 32 bits, the IRQ is 32 bits
                for i in 0..32 {
                    trace!(
                        "l2vic irqn_enable {} irqn_pending {}",
                        slot.irqn_enable(i),
                        slot.irqn_pending(i)
                    );
                    // This means the IRQ must be executed, as it
                    // was latched.
                    if slot.irqn_enable(i) && slot.irqn_pending(i) {
                        trace!("the first IRQ set was {i}");
                        irq = Some(i + (slot_no * L2VIC_NUM_SLOTS as usize));
                        break;
                    }
                }
            }

            irq

            // Releases shared state lock
        };

        // This is the l2vic irq.
        // TODO: steering. This always executes on cpu[0]
        if let Some(irq) = selected_l2vic_irq {
            info!("l2vic next - latching irqn {irq}");
            self.execute_async(irq as i32, &mut vcpus[0])
                .with_context(|| "couldn't execute pending interrupt")?;
        }

        Ok(())
    }

    /// This will effectively set this IRQ to pending, and put it in the
    /// IRQ queue.
    ///
    /// This means when the next event are checked and executed,
    /// in `next`, this event an be considered.
    ///
    /// TODO Priority and use a queue to avoid performance issues
    fn latch(&mut self, event: ExceptionNumber) -> Result<(), ActivateIRQnError> {
        trace!("l2vic latch called with event {event}");
        self.lock().latch(event)
    }

    fn execute(
        &mut self,
        vcpu_idx: usize,
        value: u64,
        irq: ExceptionNumber,
        vcpus: &mut [VCpuCore],
    ) -> Result<InterruptExecuted, ActivateIRQnError> {
        let interrupt_type = HexagonInterruptType::from(irq);
        match interrupt_type {
            HexagonInterruptType::StartInstruction => {
                thread_instructions::start(vcpu_idx, irq, value, vcpus)
            }
            HexagonInterruptType::K0lockInstruction => {
                let mut lock_state = self.k0lock_state.lock().unwrap();
                thread_instructions::lock(
                    vcpu_idx,
                    irq,
                    value,
                    vcpus,
                    HexagonLockType::K0,
                    &mut lock_state,
                )
            }
            HexagonInterruptType::K0UnlockInstruction => {
                let mut lock_state = self.k0lock_state.lock().unwrap();
                thread_instructions::unlock(
                    vcpu_idx,
                    irq,
                    value,
                    vcpus,
                    HexagonLockType::K0,
                    &mut lock_state,
                )
            }
            HexagonInterruptType::TlblockInstruction => {
                let mut lock_state = self.tlblock_state.lock().unwrap();
                thread_instructions::lock(
                    vcpu_idx,
                    irq,
                    value,
                    vcpus,
                    HexagonLockType::Tlb,
                    &mut lock_state,
                )
            }
            HexagonInterruptType::TlbUnlockInstruction => {
                let mut lock_state = self.tlblock_state.lock().unwrap();
                thread_instructions::unlock(
                    vcpu_idx,
                    irq,
                    value,
                    vcpus,
                    HexagonLockType::Tlb,
                    &mut lock_state,
                )
            }
            HexagonInterruptType::Resched => {
                thread_instructions::resched(vcpu_idx, irq, value, vcpus)
            }
            _ => unreachable!(),
        }
    }

    fn init(
        &mut self,
        cpu: &mut dyn CpuBackend,
        mmu: &mut MemoryBackend,
        config: &mut Config,
    ) -> Result<(), UnknownError> {
        let proc_cfg = config.get::<HexagonProcessorConfig>().expect("You need to provide a hexagon process configuration in processor config to initialize l2vic");
        let l2vic_cfg = &proc_cfg.l2vic_config;

        let l2vic_base = proc_cfg.subsystem_base + L2VIC_OFFSET;
        let fastl2vic_base = l2vic_cfg.fastl2vic_base;

        write_cfgtable_field(
            cpu,
            mmu,
            FASTL2VIC_CFGTABLE_OFFSET,
            (fastl2vic_base >> 16) as u32,
        );

        let mut l2vic = self.lock();
        l2vic.l2vic_base = Some(l2vic_base);
        l2vic.fastl2vic_base = Some(fastl2vic_base);
        l2vic.vid_irq_base = Some(l2vic_cfg.vid_irq_base);

        info!(
            "l2vic initializing with l2vic_base {l2vic_base:x} fastl2vic_base {fastl2vic_base:x}"
        );

        cpu.mem_write_hook(
            l2vic_base,
            l2vic_base + 0x1000,
            peripheral_shared_state_write(l2vic_mmio_write_hook, self.inner.clone()),
        )?;

        cpu.mem_read_hook(
            l2vic_base,
            l2vic_base + 0x1000,
            peripheral_shared_state_read(l2vic_mmio_read_hook, self.inner.clone()),
        )?;

        cpu.mem_write_hook(
            fastl2vic_base,
            fastl2vic_base + 0x4,
            peripheral_shared_state_write(fastl2vic_mmio_write_hook, self.inner.clone()),
        )?;

        info!("the hexagon l2vic has started");

        Ok(())
    }
}

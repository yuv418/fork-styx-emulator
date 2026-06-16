// SPDX-License-Identifier: BSD-2-Clause

//! Hexagon clade

use arbitrary_int::*;
use bitbybit::{bitenum, bitfield};
use bitvec::prelude::*;
use safe::clade1::Clade1;
use std::process::exit;
use std::sync::{Arc, Mutex};
use styx_core::hooks::StyxHook;
use styx_core::{
    arch::{
        hexagon::{
            register_fields::{Ipendad, Ssr},
            HexagonRegister,
        },
        RegisterValue,
    },
    cpu::{CpuBackend, CpuBackendExt, HexagonInterruptType},
    errors::UnknownError,
    event_controller::{ActivateIRQnError, InterruptExecuted, Peripherals},
    hooks::CoreHandle,
    macros::peripheral_shared_state,
    memory::{MemoryOperation, MemoryType, Mmu},
    prelude::{
        log::{error, info, trace, warn},
        ArchRegister, BasicArchRegister, Context, ExceptionNumber, Peripheral,
    },
};
use styx_hexagon_sys::clade;

use crate::shared_state_hooks::{peripheral_shared_state_read, peripheral_shared_state_write};

mod safe;

const CLADE_BASE: u64 = 0x57d0000;
const CLADE2_BASE: u64 = 0x55b0000;

const CLADE_REGION_SIZE: usize = 0x20000000;
const CLADE_CHUNK_SIZE: usize = 0x10000;
// Round up the number of extracted regions to make sure everything fits
// Basically, we chunk the clade extraction into regions of EXTRACT_SIZE,
// so if you have address A, you get the region A is in rounded down to EXTRACT_SIZE,
// extract this, mark it as extracted, and continue.
const CLADE_NO_CHUNKS: usize = (CLADE_REGION_SIZE + CLADE_CHUNK_SIZE - 1) / CLADE_CHUNK_SIZE;
type CladeExtractedBitvec = BitArr!(for CLADE_NO_CHUNKS, in usize, Lsb0);

#[derive(Debug)]
#[bitenum(u16, exhaustive = false)]
pub enum Clade1Register {
    // Output address (PA), shifted right by 0x1d
    OutputAddr = 0,
    // Compressed region start (PA), shifted right by 0x4
    CompressRegion = 16,
    // Hi exception start
    ExcHi = 20,
    ExcLo = 28,
    // Start of dictionary.
    DictStart = 8192,
    DictEnd = 32764,
}

/// Get the corresponding page for the address, extract it, mark as extracted.
fn clade_extract(
    proc: CoreHandle,
    addr: u64,
    mem_type: MemoryType,
    clade_state: Arc<Mutex<CladeSharedState>>,
) -> Result<(), UnknownError> {
    // We should not fail on a clade address that isn't mapped, so we can just silently bail out here.
    // Page fault handler should add a mapping and this will inevitably be called again.

    let mut periph = clade_state
        .lock()
        .expect("Couldn't get CLADE peripheral shared state");

    if let Ok(real_addr) = proc
        .mmu
        .translate_va(addr, MemoryOperation::Read, mem_type, proc.cpu)
    {
        // Get which "clade chunk" to extract. Don't extract an already extracted chunk.
        let chunk_num = (real_addr as usize - periph.output_addr as usize) / CLADE_CHUNK_SIZE;
        if !periph.extracted[chunk_num] {
            info!("CLADE extract, mem_type {mem_type:?} real_addr {real_addr:x}, output_addr {:x}, chunk num {chunk_num:x}", periph.output_addr);

            info!("CLADE request for chunk 0x{chunk_num:x}, extracting");

            let clade_chunk_output_addr =
                periph.output_addr + (chunk_num * CLADE_CHUNK_SIZE) as u64;
            periph
                .engine
                .extract(proc.mmu, clade_chunk_output_addr, CLADE_CHUNK_SIZE)?;
            periph.extracted.set(chunk_num, true);
        }
    } else {
        warn!("clade address 0x{addr:x} couldn't be translated, skipping.");
    }

    Ok(())
}

fn clade_mmio_write_hook(
    proc: CoreHandle,
    address: u64,
    size: u32,
    data: &[u8],
    clade_state: Arc<Mutex<CladeSharedState>>,
) -> Result<(), UnknownError> {
    // Needed so we can pass clade_state to the mem read hooks later.

    let mut periph = clade_state
        .lock()
        .expect("Couldn't get CLADE peripheral shared state");
    let register = Clade1Register::new_with_raw_value((address - CLADE_BASE) as u16);
    let value = u32::from_le_bytes(data.try_into().unwrap()); //  .expect("couldn't unwrap clade value");

    match register {
        Ok(Clade1Register::OutputAddr) => {
            let periph_output_addr = (value as u64) << 0x1d;

            periph.output_addr = periph_output_addr;
            periph.engine.set_output_addr(periph_output_addr);

            info!("CLADE: setting output to {:x}", periph.output_addr);

            for i in 0..periph.num_hthreads {
                let state_clone_read_hook = clade_state.clone();
                let state_clone_code_hook = clade_state.clone();

                // lowkenuinely not sure what this should be
                // TODO: delete the hook if this is changed.
                proc.event_controller.add_hook(
                    StyxHook::MemoryRead(
                        (periph.output_addr..periph.output_addr + CLADE_REGION_SIZE as u64).into(),
                        Box::new(
                            move |proc: CoreHandle, address: u64, size: u32, data: &mut [u8]| {
                                clade_extract(
                                    proc,
                                    address,
                                    MemoryType::Data,
                                    state_clone_read_hook.clone(),
                                )
                            },
                        ),
                    ),
                    i,
                );

                proc.event_controller.add_hook(
                    StyxHook::Code(
                        (periph.output_addr..(periph.output_addr + CLADE_REGION_SIZE as u64))
                            .into(),
                        Box::new(move |mut proc: CoreHandle| {
                            let pc_addr = proc.pc().expect("Couldn't get PC for extracting clade");
                            clade_extract(
                                proc,
                                pc_addr,
                                MemoryType::Code,
                                state_clone_code_hook.clone(),
                            )
                        }),
                    ),
                    i,
                );
            }
        }
        Ok(Clade1Register::CompressRegion) => {
            let compress_addr = (value as u64) << 4;

            trace!("getting clade compressed data at {compress_addr:x}",);

            periph.engine.set_compress_section(compress_addr);
        }
        // TODO: exc_lo handling, in case it is needed.
        Ok(Clade1Register::ExcHi) => {
            let exc_hi_addr = (value as u64) << 4;

            periph.engine.set_exc_hi(exc_hi_addr);
        }
        // TODO: exc_lo handling, in case it is needed.
        Ok(Clade1Register::ExcLo) => {
            let exc_lo_addr = (value as u64) << 4;

            periph.engine.set_exc_lo(exc_lo_addr);
        }
        Ok(Clade1Register::DictStart) => {
            trace!("clade dictionary being written");
        }
        Ok(Clade1Register::DictEnd) => {
            trace!("clade dictionary done being written");
            proc.mmu
                .read_data(
                    CLADE_BASE + Clade1Register::DictStart as u64,
                    &mut periph.dictionary_section,
                )
                .unwrap();

            periph.update_engine_dict_section();
        }
        Err(val) => {
            trace!(
		"clade base writing at 0x{address:x} and size 0x{size:x} with data {data:x?}, pc is {:x?} val is {val:x}",
        proc.cpu.pc()
    );
        }
    }

    Ok(())
}

fn clade_mmio_read_hook(
    proc: CoreHandle,
    address: u64,
    size: u32,
    data: &mut [u8],
    _clade_state: Arc<Mutex<CladeSharedState>>,
) -> Result<(), UnknownError> {
    // stupid hack that hexagon-sim seems to do.
    if address == CLADE2_BASE && size == 4 {
        data.fill(0);
    }

    warn!(
        "clade base reading at 0x{address:x} and size 0x{size:x} with data {data:x?}, pc is {:x?}",
        proc.cpu.pc()
    );

    Ok(())
}

#[peripheral_shared_state]
pub struct Clade {
    #[shared]
    engine: Clade1,
    #[shared]
    output_addr: u64,
    #[shared]
    dictionary_section: Vec<u8>,
    #[shared]
    exc_hi_section: Vec<u8>,
    #[shared]
    extracted: CladeExtractedBitvec,
    #[shared]
    num_hthreads: usize,
}

// Used for fixing up ownership issues
impl CladeSharedState {
    /// Using the set dictionary section, update
    /// the internal Clade engine's dictionary.
    /// Called once the clade dictionary is finished
    ///  being set through MMIO.
    fn update_engine_dict_section(&mut self) {
        self.engine.set_dict_section(&mut self.dictionary_section);
    }
}

impl Default for Clade {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(CladeSharedState {
                engine: Clade1::new(),
                output_addr: 0,
                dictionary_section: vec![0; 0x6000],
                exc_hi_section: vec![0; 0x2000],
                extracted: bitarr![0; CLADE_NO_CHUNKS],
                num_hthreads: 0,
            })),
        }
    }
}

impl Peripheral for Clade {
    fn name(&self) -> &str {
        "Qualcomm Clade Decompression Engine"
    }

    fn init(
        &mut self,
        proc: &mut styx_core::prelude::BuildingProcessor,
    ) -> Result<(), styx_core::prelude::UnknownError> {
        unsafe { clade::clade_set_trace(0xff) };

        let mut clade_inner = self.lock();
        clade_inner.num_hthreads = proc.vcpus.len();

        for vcpu in proc.vcpus.iter_mut() {
            vcpu.cpu
                .mem_write_hook(
                    CLADE_BASE,
                    CLADE_BASE + 0x8000,
                    peripheral_shared_state_write(clade_mmio_write_hook, self.inner.clone()),
                )
                .with_context(|| "couldn't add MMIO hooks for clade")?;

            vcpu.cpu
                .mem_read_hook(
                    CLADE_BASE,
                    CLADE_BASE + 0x8000,
                    peripheral_shared_state_read(clade_mmio_read_hook, self.inner.clone()),
                )
                .with_context(|| "couldn't add MMIO hooks for clade")?;

            /*proc.core
            .cpu
            .mem_write_hook(
                CLADE2_BASE,
                CLADE2_BASE + 0x8000,
                Box::new(clade_mmio_write_hook),
            )
            .with_context(|| "couldn't add MMIO hooks for clade")?;*/
            vcpu.cpu
                .mem_read_hook(
                    CLADE2_BASE,
                    CLADE2_BASE + 0x8000,
                    peripheral_shared_state_read(clade_mmio_read_hook, self.inner.clone()),
                )
                .with_context(|| "couldn't add MMIO hooks for clade")?;
        }
        Ok(())
    }
}

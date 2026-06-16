// SPDX-License-Identifier: BSD-2-Clause
//! Qcom prng peripheral. from linux drivers/crypto/qcom-rng.c

use std::sync::{Arc, Mutex};

use styx_core::{
    errors::UnknownError,
    hooks::CoreHandle,
    macros::peripheral_shared_state,
    prelude::{log::trace, BuildingProcessor, Context, Peripheral},
};

use crate::{
    shared_state_hooks::{peripheral_shared_state_read, peripheral_shared_state_write},
    HexagonProcessorConfig,
};
use bitbybit::bitenum;
use rand::{prelude::*, rngs::SysRng};

const DATA_AVAILABLE: u32 = 1;
const HW_ENABLE: u32 = 1 << 1;
const SEED: u64 = 0xdeadbeef;

#[derive(Debug)]
#[bitenum(u16, exhaustive = false)]
pub enum QcomPrngRegister {
    DataOut = 0x0,
    Status = 0x4,
    LfsrConfig = 0x100,
    Config = 0x104,
}

fn prng_mmio_write_hook(
    proc: CoreHandle,
    address: u64,
    size: u32,
    data: &[u8],
    timer_state_mutex: Arc<Mutex<QcomPrngSharedState>>,
) -> Result<(), UnknownError> {
    unimplemented!("prng write unimpl");
}

fn prng_mmio_read_hook(
    proc: CoreHandle,
    address: u64,
    size: u32,
    data: &mut [u8],
    timer_state_mutex: Arc<Mutex<QcomPrngSharedState>>,
) -> Result<(), UnknownError> {
    let mut ts = timer_state_mutex.lock().unwrap();
    let off = address - ts.base_addr.unwrap();

    match QcomPrngRegister::new_with_raw_value(off as u16) {
        Ok(QcomPrngRegister::DataOut) => {
            trace!("generating random number for qcom prng");
            for i in 0..(size as usize) {
                let rand_val = ts.rng.random::<u8>();
                data[i] = rand_val;
            }
            trace!("generated {:x?}", data);

            Ok(())
        }
        Ok(QcomPrngRegister::Status) => {
            // should already be written
            trace!(
                "prng status read: {:x}",
                u32::from_le_bytes(data.try_into().unwrap())
            );
            Ok(())
        }
        Ok(QcomPrngRegister::LfsrConfig) => {
            unimplemented!()
        }
        Ok(QcomPrngRegister::Config) => {
            unimplemented!()
        }
        Err(e) => panic!("accessed qcom prng reg {e:x}"),
    }
}

#[peripheral_shared_state]
pub struct QcomPrng {
    #[shared]
    base_addr: Option<u64>,
    #[shared]
    rng: StdRng,
}

impl Default for QcomPrng {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(QcomPrngSharedState {
                base_addr: None,
                rng: StdRng::seed_from_u64(SEED),
            })),
        }
    }
}

impl Peripheral for QcomPrng {
    fn name(&self) -> &str {
        "qualcomm pseudo random number generator"
    }

    fn init(&mut self, proc: &mut BuildingProcessor) -> Result<(), UnknownError> {
        let proc_cfg = proc.config.get::<HexagonProcessorConfig>().expect("You need to provide a hexagon process configuration in processor config to initialize qtimer");
        let prng_cfg = &proc_cfg.prng_config;

        self.lock().base_addr = Some(prng_cfg.base_addr);

        proc.vcpus[0]
            .mmu
            .write_u32_le_phys_data(
                prng_cfg.base_addr + QcomPrngRegister::Status as u64,
                DATA_AVAILABLE | HW_ENABLE,
            )
            .unwrap();

        for vcpu in proc.vcpus.iter_mut() {
            vcpu.cpu
                .mem_write_hook(
                    prng_cfg.base_addr,
                    prng_cfg.base_addr + 0x1000,
                    peripheral_shared_state_write(prng_mmio_write_hook, self.inner.clone()),
                )
                .with_context(|| "couldn't add MMIO hooks for prng")?;

            vcpu.cpu
                .mem_read_hook(
                    prng_cfg.base_addr,
                    prng_cfg.base_addr + 0x1000,
                    peripheral_shared_state_read(prng_mmio_read_hook, self.inner.clone()),
                )
                .with_context(|| "couldn't add MMIO hooks for prng")?;
        }

        Ok(())
    }
}

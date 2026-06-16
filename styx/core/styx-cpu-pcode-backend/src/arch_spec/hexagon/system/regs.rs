// SPDX-License-Identifier: BSD-2-Clause

use log::info;
use styx_cpu_type::arch::{
    backends::{ArchRegister, SpecialArchRegister},
    hexagon::{register_fields::Ssr, BadVaRegister, HexagonRegister, SpecialHexagonRegister},
    RegisterValue,
};
use styx_errors::{anyhow::Context, UnknownError};
use styx_pcode_translator::sla::Hexagon;
use styx_processor::{
    cpu::CpuBackendExt,
    hooks::{CoreHandle, Hookable, StyxHook},
};

use crate::HexagonPcodeBackend;

pub fn add_regs_handlers(backend: &mut HexagonPcodeBackend) {
    backend
        .add_hook(StyxHook::RegisterRead(
            SpecialHexagonRegister::BadVaRegister(BadVaRegister::new()).into(),
            Box::new(
                |proc: CoreHandle,
                 register: ArchRegister,
                 data: &mut RegisterValue|
                 -> Result<(), UnknownError> {
                    info!("hello from register read hook register {register:?}");

                    // Skip if unhooked
                    let unhooked = ArchRegister::Special(SpecialArchRegister::Hexagon(
                        SpecialHexagonRegister::BadVaRegister(BadVaRegister::new_unhooked()),
                    ));
                    info!(
                        "left {:?} unhooked is {unhooked:?} matches {} equal {}",
                        register,
                        matches!(register, unhooked),
                        register == unhooked
                    );
                    if register == unhooked {
                        info!("bye");
                        return Ok(());
                    }

                    let ssr = Ssr::new_with_raw_value(
                        proc.cpu
                            .read_register::<u32>(HexagonRegister::Ssr)
                            .with_context(|| "couldn't read ssr register for badva")?,
                    );

                    let value = if ssr.bvs() {
                        info!("reading badva1");
                        proc.cpu
                            .read_register::<u32>(HexagonRegister::BadVa1)
                            .with_context(|| "couldn't read badva1 register for badva")?
                    } else {
                        info!("reading badva0");
                        proc.cpu
                            .read_register::<u32>(HexagonRegister::BadVa0)
                            .with_context(|| "couldn't read badva0 register for badva")?
                    };

                    *data = value.into();

                    Ok(())
                },
            ),
        ))
        .expect("Couldn't add badva register handler");
}

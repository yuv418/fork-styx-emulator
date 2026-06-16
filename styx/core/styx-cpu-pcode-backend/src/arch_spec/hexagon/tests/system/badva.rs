// SPDX-License-Identifier: BSD-2-Clause
use log::trace;
use styx_cpu_type::{
    arch::hexagon::{GlobalHexagonRegister, HexagonRegister},
    TargetExitReason,
};
use styx_processor::{
    cpu::{CpuBackend, CpuBackendExt},
    hooks::{CoreHandle, Hookable, StyxHook},
};

use crate::arch_spec::hexagon::tests::setup_objdump;

/// Test if setting ssr.bvs will
#[test]
fn test_badva() {
    let (mut cpu, mut mmu, mut ev) = setup_objdump(
        r#"
       0:	00 f6 05 78	7805f600 { 	r0 = #0xbb0 }
       4:	04 c0 00 67	6700c004 { 	badva0 = r0 }
       8:	e0 f6 03 78	7803f6e0 { 	r0 = #0x7b7 }
       c:	05 c0 00 67	6700c005 { 	badva1 = r0 }
      10:	00 40 04 00	00044000 { 	immext(#0x400000)
      14:	00 c0 00 78	7800c000   	r0 = ##0x400000 }
      18:	06 c0 00 67	6700c006 { 	ssr = r0 }
      1c:	01 c0 89 6e	6e89c001 { 	r1 = badva }
      20:	00 c0 00 78	7800c000 { 	r0 = #0x0 }
      24:	06 c0 00 67	6700c006 { 	ssr = r0 }
      28:	02 c0 89 6e	6e89c002 { 	r2 = badva }
"#,
    );

    let res = cpu.execute(&mut mmu, &mut ev, 10).unwrap();
    assert_eq!(res.exit_reason, TargetExitReason::InstructionCountComplete);

    // BVS set when r1 read, expect badva1
    // BVS not set when r2 read, expect badva0

    let r1 = cpu.read_register::<u32>(HexagonRegister::R1).unwrap();
    let r2 = cpu.read_register::<u32>(HexagonRegister::R2).unwrap();

    assert_eq!(r1, 0x7b7);
    assert_eq!(r2, 0xbb0);
}

// SPDX-License-Identifier: BSD-2-Clause

use crate::arch_spec::hexagon::tests::*;
use log::info;
use styx_cpu_type::arch::hexagon::register_fields::Usr;
use styx_processor::cpu::ExecutionReport;
use test_case::test_case;

#[test_case(773.292f64, 773; "773")]
pub fn test_abs_helper(inp: f64, expect: u32) {
    let (mut cpu, mut mmu, mut ev) = setup_objdump(
        r#"
	0:	21 c0 a2 88	88a2c021 { 	r1 = convert_df2uw(r3:2):chop }
"#,
    );

    cpu.write_register(HexagonRegister::D1, inp.to_bits())
        .unwrap();

    let exit = cpu.execute(&mut mmu, &mut ev, 1).unwrap();
    assert_eq!(exit.exit_reason, TargetExitReason::InstructionCountComplete);

    let r1 = cpu.read_register::<u32>(HexagonRegister::R1).unwrap();
    assert_eq!(r1, expect);
}

#[test_case(773.292f64, 2.331f64; "mul1")]
#[test_case(0.0f64, f64::from_bits(0x4033333333333333u64); "mul2")]
pub fn test_float_helper(inp: f64, inp1: f64) {
    let (mut cpu, mut mmu, mut ev) = setup_objdump(
        r#"
	0:	62 c6 84 ea	ea84c662 { 	r3:2 += dfmpyhh(r5:4,r7:6) }
"#,
    );

    cpu.write_register(HexagonRegister::D2, inp.to_bits())
        .unwrap();
    cpu.write_register(HexagonRegister::D3, inp.to_bits())
        .unwrap();

    let exit = cpu.execute(&mut mmu, &mut ev, 1).unwrap();
    assert_eq!(exit.exit_reason, TargetExitReason::InstructionCountComplete);

    let r1 = f64::from_bits(cpu.read_register::<u64>(HexagonRegister::D1).unwrap());
    assert_eq!(r1, 0.0f64);
}

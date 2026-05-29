// SPDX-License-Identifier: BSD-2-Clause
//! Regression test for `nexti`/`stepi` over a PowerPC `bl` (call) in the GDB plugin.
//!
//! The gdb client can only recognize a `bl` as a subroutine call if it can
//! unwind the call stack, which needs (a) firmware that follows the PowerPC
//! stack conventions (the unified firmware does) and (b) function-boundary
//! symbols (loaded here from the assembled object, `FIRMWARE_OBJ_FILE`).
//!
//! The tests step over `bl incr_private` (not `incr_shared`): `incr_private`
//! writes a *per-core* word, so stepping over it advances exactly one core's
//! counter by one -- a deterministic check even on a multi-vCPU target.
use gdb_multicore_ppc::{
    DualPpc405Builder, SinglePpc405Builder, BL_OFFSET, CODE_BASE_0, CODE_BASE_1, FIRMWARE,
    PRIV_BASE_0, PRIV_SUB_OFFSET, RETURN_OFFSET,
};
use styx_emulator::arch::ppc32::gdb_targets::Ppc4xxTargetDescription;
use styx_emulator::plugins::gdb::{GDBOptions, StepIRQs};
use styx_emulator::prelude::*;
use styx_integration_tests::gdb_harness::GdbHarness;

/// Path to the object file holding the firmware's symbols, assembled from
/// `firmware/firmware.S` at build time by `build.rs` (see `FIRMWARE_OBJ_FILE`).
fn symbol_file() -> String {
    env!("FIRMWARE_OBJ_FILE").to_string()
}

/// Read a big-endian u32 from target memory through GDB.
fn read_u32(h: &GdbHarness, addr: u64) -> u32 {
    let bytes = h.read_memory(addr, 4).unwrap();
    u32::from_be_bytes(bytes.try_into().unwrap())
}

fn single_core_harness() -> GdbHarness {
    let builder = ProcessorBuilder::default().with_builder(SinglePpc405Builder::default());
    GdbHarness::from_processor_builder_options::<Ppc4xxTargetDescription>(
        builder,
        GDBOptions {
            step_irqs: StepIRQs::Disabled,
            cpu_epoch: 1024,
        },
    )
}

fn dual_core_harness() -> GdbHarness {
    let builder = ProcessorBuilder::default().with_builder(DualPpc405Builder::default());
    GdbHarness::from_processor_builder_options::<Ppc4xxTargetDescription>(
        builder,
        GDBOptions {
            step_irqs: StepIRQs::Disabled,
            cpu_epoch: 1024,
        },
    )
}

/// `stepi` on the `bl` must step *into* the subroutine (sanity check that the
/// firmware really does call and that plain single-step lands in the callee).
#[test]
#[cfg_attr(miri, ignore)]
fn test_stepi_steps_into_call() {
    let h = single_core_harness();
    h.add_symbol_file(&symbol_file(), CODE_BASE_0).unwrap();
    let bl = CODE_BASE_0 + BL_OFFSET;
    h.add_breakpoint(bl).unwrap();
    h.gdb_continue().unwrap();
    let stopped = h.wait_for_stop().unwrap();
    assert_eq!(stopped.address.0, bl, "should break on the bl instruction");

    let pc = h.step_instruction().unwrap();
    assert_eq!(
        pc,
        CODE_BASE_0 + PRIV_SUB_OFFSET,
        "stepi over a `bl` should land at the subroutine entry"
    );
}

/// `nexti` on the `bl` must step *over* the subroutine and land on the
/// instruction after the call. Single-vCPU case. Also asserts the per-core
/// private counter advanced by exactly one (proof the call really executed).
#[test]
#[cfg_attr(miri, ignore)]
fn test_nexti_steps_over_call_single_core() {
    let h = single_core_harness();
    h.add_symbol_file(&symbol_file(), CODE_BASE_0).unwrap();
    let bl = CODE_BASE_0 + BL_OFFSET;
    h.add_breakpoint(bl).unwrap();
    h.gdb_continue().unwrap();
    let stopped = h.wait_for_stop().unwrap();
    assert_eq!(stopped.address.0, bl, "should break on the bl instruction");

    let before = read_u32(&h, PRIV_BASE_0);
    let pc = h.next_instruction().unwrap();
    assert_eq!(
        pc,
        CODE_BASE_0 + RETURN_OFFSET,
        "nexti over a `bl` should land at the return target (call+4), not step into the subroutine \
         (got 0x{pc:x}; subroutine entry is 0x{:x})",
        CODE_BASE_0 + PRIV_SUB_OFFSET
    );
    let after = read_u32(&h, PRIV_BASE_0);
    assert_eq!(
        after,
        before + 1,
        "stepping over `bl incr_private` must run the call exactly once \
         (private counter {before} -> {after})"
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_step_reports_stepping_thread() {
    let h = dual_core_harness();

    // Step *thread 2* (core 1).
    h.select_thread(2).unwrap();
    h.step_instruction().unwrap();

    // The stepped thread must remain the current/stopped thread.
    assert_eq!(
        h.current_thread().unwrap(),
        2,
        "after stepping thread 2 the stopped thread must still be thread 2, \
         not snap back to thread 1 (bare `S05` stop-reply with no thread id)"
    );

    // Each core must report its *own* PC, within its own code block. Both cores
    // run the same firmware, just at different bases.
    h.select_thread(2).unwrap();
    let pc_t2 = h.list_registers().unwrap()["pc"];
    assert!(
        (CODE_BASE_1..CODE_BASE_1 + FIRMWARE.len() as u64).contains(&pc_t2),
        "core 1's PC (0x{pc_t2:x}) must stay in its own code block at 0x{CODE_BASE_1:x}"
    );

    h.select_thread(1).unwrap();
    let pc_t1 = h.list_registers().unwrap()["pc"];
    assert!(
        (CODE_BASE_0..CODE_BASE_0 + FIRMWARE.len() as u64).contains(&pc_t1),
        "core 0's PC (0x{pc_t1:x}) must stay in its own code block, not pick up \
         core 1's PC (0x{CODE_BASE_1:x})"
    );
}

/// Same as the single-core `nexti` test but on a *multi-vCPU* target. Core 0
/// hits its own `bl`; core 1's identical `bl` sits at a different base, so it
/// never trips core 0's breakpoint. The per-core counter check stays
/// deterministic because `incr_private` only touches core 0's `PRIV_BASE_0`.
#[test]
#[cfg_attr(miri, ignore)]
fn test_nexti_steps_over_call_multi_core() {
    let h = dual_core_harness();
    h.add_symbol_file(&symbol_file(), CODE_BASE_0).unwrap();
    let bl = CODE_BASE_0 + BL_OFFSET;
    h.add_breakpoint(bl).unwrap();
    h.gdb_continue().unwrap();
    let stopped = h.wait_for_stop().unwrap();
    assert_eq!(stopped.address.0, bl, "should break on the bl instruction");
    assert_eq!(
        h.current_thread().unwrap(),
        1,
        "core 0 (thread 1) should hit the bl"
    );

    let before = read_u32(&h, PRIV_BASE_0);
    let pc = h.next_instruction().unwrap();
    assert_eq!(
        pc,
        CODE_BASE_0 + RETURN_OFFSET,
        "nexti over a `bl` should land at the return target (call+4), not step into the subroutine \
         (got 0x{pc:x}; subroutine entry is 0x{:x})",
        CODE_BASE_0 + PRIV_SUB_OFFSET
    );
    let after = read_u32(&h, PRIV_BASE_0);
    assert_eq!(
        after,
        before + 1,
        "stepping over core 0's `bl incr_private` must advance only core 0's \
         private counter, by one ({before} -> {after})"
    );
}

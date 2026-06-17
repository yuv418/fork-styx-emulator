// SPDX-License-Identifier: BSD-2-Clause
//! Automated multi-core GDB tests for the two-core PPC405 example.
use gdb_multicore_ppc::{
    DualPpc405Builder, CODE_BASE_0, CODE_BASE_1, PRIV_BASE_0, PRIV_BASE_1, PRIV_STW_OFFSET,
    SHARED_COUNTER,
};
use styx_emulator::arch::ppc32::gdb_targets::Ppc4xxTargetDescription;
use styx_emulator::plugins::gdb::{GDBOptions, StepIRQs};
use styx_emulator::prelude::*;
use styx_integration_tests::gdb_harness::{self, GdbHarness};

/// Build a harness around the two-core processor.
fn harness() -> GdbHarness {
    let builder = ProcessorBuilder::default().with_builder(DualPpc405Builder::default());
    let harness = GdbHarness::from_processor_builder_options::<Ppc4xxTargetDescription>(
        builder,
        GDBOptions {
            step_irqs: StepIRQs::Disabled,
            cpu_epoch: 1024,
        },
    );
    // Styx only supports `off`.
    harness
        .set_scheduler_locking(gdb_harness::SchedulerLockingMode::Off)
        .unwrap();
    harness
}

/// Read a big-endian u32 from target memory through GDB.
fn read_u32(h: &GdbHarness, addr: u64) -> u32 {
    let bytes = h.read_memory(addr, 4).unwrap();
    u32::from_be_bytes(bytes.try_into().unwrap())
}

#[test]
fn test_two_threads_present() {
    let h = harness();
    let threads = h.list_threads().unwrap();
    assert_eq!(
        threads.len(),
        2,
        "expected 2 gdb threads (cores), got {threads:?}"
    );
}

#[test]
fn test_cores_distinct() {
    let h = harness();
    h.select_thread(1).unwrap();
    let regs_t1 = h.list_registers().unwrap();
    let (r3_t1, pc_t1) = (regs_t1["r3"], regs_t1["pc"]);
    h.select_thread(2).unwrap();
    let regs_t2 = h.list_registers().unwrap();
    let (r3_t2, pc_t2) = (regs_t2["r3"], regs_t2["pc"]);

    // Each core is preset with its own private base in r3...
    assert_eq!(r3_t1, PRIV_BASE_0, "core 0 private base");
    assert_eq!(r3_t2, PRIV_BASE_1, "core 1 private base");
    assert_ne!(r3_t1, r3_t2, "cores must have distinct private bases");

    // ...and runs its own copy of the firmware, so each pc sits in its own code
    // block (core 0 at CODE_BASE_0, core 1 at CODE_BASE_1).
    assert_eq!(
        pc_t1, CODE_BASE_0,
        "core 0 should start in its own code copy"
    );
    assert_eq!(
        pc_t2, CODE_BASE_1,
        "core 1 should start in its own code copy"
    );
}

#[test]
fn test_breakpoint_hits_core_1() {
    let h = harness();
    // The `stw` in core 1's *own* firmware copy lives only at this address, so
    // the breakpoint can be hit by core 1 (thread 2) and no other core. Proves
    // breaking works on a non-zero core, not just core 0.
    let stw1 = CODE_BASE_1 + PRIV_STW_OFFSET;
    h.add_breakpoint(stw1).unwrap();
    // Run round-robin until core 1 reaches its `stw`. Core 0 has no breakpoint,
    // so the only stop this can produce is core 1 hitting `stw1`.
    h.gdb_continue().unwrap();
    let stopped = h.wait_for_stop().unwrap();
    assert_eq!(stopped.address.0, stw1, "stopped at the wrong address");
    assert_eq!(
        h.current_thread().unwrap(),
        2,
        "only core 1 (thread 2) should hit core 1's breakpoint"
    );
}

/// Test stepping the second core (vcpu 1) instead of the first.
#[test]
#[cfg_attr(miri, ignore)]
fn test_stepi_advances_core_1() {
    let h = harness();

    // Single-step with core 1 (thread 2) selected.
    h.select_thread(2).unwrap();
    let pc = h.step_instruction().unwrap();
    // A single `stepi` must advance core 1's PC by exactly one word.
    assert_eq!(
        pc,
        CODE_BASE_1 + 4,
        "stepi on core 1 should advance its PC one instruction"
    );
    // The stepped thread must remain the current/stopped thread.
    assert_eq!(
        h.current_thread().unwrap(),
        2,
        "after stepping thread 2 the stopped thread must still be thread 2"
    );
    // Check from a read register for good measure.
    h.select_thread(2).unwrap();
    assert_eq!(
        h.list_registers().unwrap()["pc"],
        CODE_BASE_1 + 4,
        "core 1's pc register should reflect its single step"
    );

    // Check tid 1 already progressed (set scheduler-locking off)
    // **technically** tid 1 can progress any amount of instructions,
    // so future gdbserver implementations may require this check
    // to change.
    h.select_thread(1).unwrap();
    assert_eq!(
        h.list_registers().unwrap()["pc"],
        CODE_BASE_0 + 4,
        "core 1's pc register should reflect its single step"
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_private_writes_independent() {
    let h = harness();
    // Break on *core 1*'s private `stw`.
    // Because we run round-robin, the first breakpoint hit on vcpu 0 will not
    // have run vcpu1 yet. After continuing the second time it will be run a
    // full epoch.
    let stw1 = CODE_BASE_1 + PRIV_STW_OFFSET;
    h.add_breakpoint(stw1).unwrap();
    h.gdb_continue().unwrap();
    h.wait_for_stop().unwrap();
    h.gdb_continue().unwrap();
    h.wait_for_stop().unwrap();
    let p0 = read_u32(&h, PRIV_BASE_0);
    let p1 = read_u32(&h, PRIV_BASE_1);
    assert!(p0 > 0, "core 0 private counter should be nonzero");
    assert!(p1 > 0, "core 1 private counter should be nonzero");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_shared_counter_increments() {
    let h = harness();
    let stw0 = CODE_BASE_0 + PRIV_STW_OFFSET;
    h.add_breakpoint(stw0).unwrap();
    h.gdb_continue().unwrap();
    h.wait_for_stop().unwrap();
    let s1 = read_u32(&h, SHARED_COUNTER);
    h.gdb_continue().unwrap();
    h.wait_for_stop().unwrap();
    let s2 = read_u32(&h, SHARED_COUNTER);
    assert!(s2 > s1, "shared counter should increase: {s1} -> {s2}");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_memory_watchpoint() {
    let h = harness();
    // Core 0 writes its private word every loop iteration, so a watchpoint on it
    // should fire quickly.
    let wp = h.add_watchpoint(PRIV_BASE_0).unwrap();
    assert!(
        h.list_watchpoints().unwrap().contains(&wp),
        "watchpoint should be registered"
    );
    h.gdb_continue().unwrap();
    let _stopped = h.wait_for_stop().unwrap();
    assert!(matches!(
        _stopped.reason.unwrap(),
        gdb_harness::StopReason::Watchpoint { number: _ }
    ));
}

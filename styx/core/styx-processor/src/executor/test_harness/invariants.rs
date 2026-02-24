// SPDX-License-Identifier: BSD-2-Clause
//! Universal correctness invariants checked against a recorded trace.

use crate::executor::test_harness::scenario::DriveOutcome;
use crate::executor::test_harness::trace::{ExecutorEvent, ExecutorTrace};

/// Snapshot of processor state captured immediately after the scenario's
/// `drive()` returns. Used to validate that the trace totals match reality.
#[derive(Debug, Clone)]
pub struct PostRunSnapshot {
    pub core_simulated: u64,
    pub vcpu_simulated: Vec<u64>,
}

/// Reason a universal invariant failed. All variants carry enough context to
/// diagnose the executor under test.
#[derive(Debug)]
pub enum InvariantViolation {
    /// `sum(SystemTick.delta.simulated)` disagrees with `core.simulated_time()`.
    CoreTimeMismatch { trace_sum: u64, core_simulated: u64 },
    /// A SystemTick was recorded with no VcpuTick between it and the previous
    /// SystemTick (or the start of the trace) — i.e. `post_stride_processing`
    /// was not called for any vCPU before the event distributor ticked.
    MissingPostStrideProcessing { tick_index: usize },
    /// Total simulated cycles fell outside `[expected - tol, expected + tol]`.
    TotalCyclesOutOfTolerance {
        expected: u64,
        tolerance: u64,
        actual: u64,
    },
    /// `DriveOutcome::expected_system_ticks` did not match the recorded count.
    UnexpectedTickCount { expected: usize, actual: usize },
}

/// Check the universal correctness invariants against a recorded trace and a
/// post-run state snapshot.
///
/// Invariants checked:
/// 1. `sum(SystemTick.delta.simulated) == snapshot.core_simulated`.
/// 2. Every SystemTick is preceded (since the last SystemTick or trace start)
///    by at least one VcpuTick which proves the executor called
///    `post_stride_processing` at least once per round.
/// 3. Total simulated cycles falls within
///    `outcome.expected_total_cycles +- tolerance`.
/// 4. If `outcome.expected_system_ticks.is_some()`, count matches exactly.
///
/// Per-vCPU checks (e.g. "every vCPU ticked every round") are deliberately
/// **not** universal: the round-robin executor legitimately skips
/// `post_stride_processing` for a vCPU that fatally exits mid-stride, and
/// ghost-advances exited vCPUs' `simulated_time` in later rounds. Both
/// break any naive per-vCPU trace-vs-snapshot equality. Scenarios that want
/// tighter per-vCPU assertions should encode them in
/// [`ExecutorTestScenario::validate`] using [`ExecutorTrace::vcpu_ticks`].
///
/// [`ExecutorTestScenario::validate`]: crate::executor::test_harness::scenario::ExecutorTestScenario::validate
pub fn verify_universal_invariants(
    trace: &ExecutorTrace,
    snapshot: &PostRunSnapshot,
    outcome: &DriveOutcome,
) -> Result<(), InvariantViolation> {
    // (1) Core time consistency.
    let trace_sum = trace.total_system_simulated();
    if trace_sum != snapshot.core_simulated {
        return Err(InvariantViolation::CoreTimeMismatch {
            trace_sum,
            core_simulated: snapshot.core_simulated,
        });
    }

    // (2) Post-stride-processing coverage: every SystemTick must have been
    // preceded by at least one VcpuTick in its window.
    let mut window_vcpu_ticks = 0usize;
    let mut tick_index = 0;
    for entry in trace.entries() {
        match &entry.event {
            ExecutorEvent::VcpuTick { .. } => {
                window_vcpu_ticks += 1;
            }
            ExecutorEvent::SystemTick { .. } => {
                if window_vcpu_ticks == 0 {
                    return Err(InvariantViolation::MissingPostStrideProcessing { tick_index });
                }
                window_vcpu_ticks = 0;
                tick_index += 1;
            }
            ExecutorEvent::Mark { .. } => {}
        }
    }

    // (3) Total cycles within tolerance.
    let diff = outcome
        .expected_total_cycles
        .abs_diff(snapshot.core_simulated);
    if diff > outcome.cycle_tolerance {
        return Err(InvariantViolation::TotalCyclesOutOfTolerance {
            expected: outcome.expected_total_cycles,
            tolerance: outcome.cycle_tolerance,
            actual: snapshot.core_simulated,
        });
    }

    // (4) Optional exact tick count.
    if let Some(expected) = outcome.expected_system_ticks {
        let actual = trace.system_ticks().count();
        if actual != expected {
            return Err(InvariantViolation::UnexpectedTickCount { expected, actual });
        }
    }

    // Snapshot per-vCPU state is read but intentionally not compared to the
    // trace — see the function-level doc for why.
    let _ = &snapshot.vcpu_simulated;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::test_harness::scenario::DriveOutcome;
    use crate::executor::test_harness::trace::{ExecutorEvent, TraceRecorder};
    use crate::executor::time::GlobalDelta;
    use crate::executor::Delta;
    use std::time::Duration;

    fn make_outcome(total: u64, tol: u64, ticks: Option<usize>) -> DriveOutcome {
        DriveOutcome {
            expected_total_cycles: total,
            cycle_tolerance: tol,
            expected_system_ticks: ticks,
            extra: Box::new(()),
        }
    }

    fn push_tick(rec: &TraceRecorder, n: u64) {
        rec.record(ExecutorEvent::SystemTick {
            delta: GlobalDelta::new(n, Duration::ZERO),
        });
    }

    fn push_vtick(rec: &TraceRecorder, v: usize, n: u64) {
        rec.record(ExecutorEvent::VcpuTick {
            vcpu: v,
            delta: Delta {
                time: Duration::ZERO,
                count: n,
            },
        });
    }

    #[test]
    fn happy_path_passes() {
        let rec = TraceRecorder::default();
        // 5 rounds, 1000 cycles each, 1 vCPU
        for _ in 0..5 {
            push_vtick(&rec, 0, 1000);
            push_tick(&rec, 1000);
        }
        let trace = rec.finish();
        let snap = PostRunSnapshot {
            core_simulated: 5000,
            vcpu_simulated: vec![5000],
        };
        let outcome = make_outcome(5000, 0, Some(5));
        verify_universal_invariants(&trace, &snap, &outcome).unwrap();
    }

    #[test]
    fn core_time_mismatch_fails() {
        let rec = TraceRecorder::default();
        push_vtick(&rec, 0, 1000);
        push_tick(&rec, 1000);
        let trace = rec.finish();
        // Snapshot claims 5000 but trace only sums to 1000.
        let snap = PostRunSnapshot {
            core_simulated: 5000,
            vcpu_simulated: vec![5000],
        };
        let outcome = make_outcome(5000, 0, None);
        let err = verify_universal_invariants(&trace, &snap, &outcome).unwrap_err();
        assert!(
            matches!(err, InvariantViolation::CoreTimeMismatch { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn missing_post_stride_processing_fails() {
        // System tick with no preceding VcpuTick.
        let rec = TraceRecorder::default();
        push_tick(&rec, 1000);
        let trace = rec.finish();
        let snap = PostRunSnapshot {
            core_simulated: 1000,
            vcpu_simulated: vec![0],
        };
        let outcome = make_outcome(1000, 0, None);
        let err = verify_universal_invariants(&trace, &snap, &outcome).unwrap_err();
        assert!(matches!(
            err,
            InvariantViolation::MissingPostStrideProcessing { .. }
        ));
    }

    #[test]
    fn per_vcpu_drift_after_exit_is_tolerated() {
        // Simulates the ScriptedExitScenario pattern: 2 vCPUs, vcpu 0 ticks
        // only in round 1 (then exits), vcpu 1 ticks in both rounds. vcpu 0's
        // simulated_time nevertheless shows 2000 due to ghost-advance. This
        // must pass — the universal layer deliberately ignores per-vcpu drift.
        let rec = TraceRecorder::default();
        push_vtick(&rec, 0, 1000);
        push_vtick(&rec, 1, 1000);
        push_tick(&rec, 1000);
        push_vtick(&rec, 1, 1000);
        push_tick(&rec, 1000);
        let trace = rec.finish();
        let snap = PostRunSnapshot {
            core_simulated: 2000,
            vcpu_simulated: vec![2000, 2000],
        };
        let outcome = make_outcome(2000, 0, Some(2));
        verify_universal_invariants(&trace, &snap, &outcome).unwrap();
    }

    #[test]
    fn expected_tick_count_enforced() {
        let rec = TraceRecorder::default();
        for _ in 0..3 {
            push_vtick(&rec, 0, 1000);
            push_tick(&rec, 1000);
        }
        let trace = rec.finish();
        let snap = PostRunSnapshot {
            core_simulated: 3000,
            vcpu_simulated: vec![3000],
        };
        let outcome = make_outcome(3000, 0, Some(5));
        let err = verify_universal_invariants(&trace, &snap, &outcome).unwrap_err();
        assert!(matches!(
            err,
            InvariantViolation::UnexpectedTickCount { .. }
        ));
    }

    #[test]
    fn tolerance_allows_small_mismatch() {
        let rec = TraceRecorder::default();
        push_vtick(&rec, 0, 950);
        push_tick(&rec, 950);
        let trace = rec.finish();
        let snap = PostRunSnapshot {
            core_simulated: 950,
            vcpu_simulated: vec![950],
        };
        // Expected 1000, tolerance 100, actual 950 → passes
        let outcome = make_outcome(1000, 100, None);
        verify_universal_invariants(&trace, &snap, &outcome).unwrap();
    }
}

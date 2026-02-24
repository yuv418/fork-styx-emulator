// SPDX-License-Identifier: BSD-2-Clause
//! Scenario trait and top-level `run_executor_test` entry point.

use std::any::Any;

use crate::executor::test_harness::builder::{TestProcessor, TestProcessorBuilder};
use crate::executor::test_harness::trace::{ExecutorTrace, TraceRecorder};

/// What the scenario expects in aggregate once `drive()` returns.
///
/// Cadence (how many ticks and how big each one) is scenario-specific. Stride
/// executors fill in `expected_system_ticks = Some(N)`; breakpoint-driven
/// executors (GDB) leave it `None` and rely on the universal invariants plus a
/// totals-within-tolerance check.
pub struct DriveOutcome {
    /// Expected total simulated cycles advanced across the whole run. The
    /// harness checks `abs(sum(system_ticks.simulated) - expected) <= tolerance`.
    pub expected_total_cycles: u64,

    /// Allowed absolute deviation from `expected_total_cycles`. Zero for
    /// deterministic stride scenarios. For breakpoint scenarios, set this to
    /// at least one stride so a mid-stride stop doesn't fail the check.
    pub cycle_tolerance: u64,

    /// If `Some(n)`, the harness additionally checks that exactly `n` system
    /// ticks occurred. Leave `None` to skip this check (gdb/fuzz cadence).
    pub expected_system_ticks: Option<usize>,

    /// Scenario-local state forwarded to `validate()`.
    pub extra: Box<dyn Any>,
}

/// A plug-in defining what to run and what to assert. Implement one of these
/// per executor-under-test and hand it to `run_executor_test`.
pub trait ExecutorTestScenario {
    /// Configure the builder and return a built [`TestProcessor`]. The
    /// harness hands in a builder with the trace recorder already installed;
    /// the scenario adds vCPUs, CPU backends, and the executor under test.
    fn configure(&mut self, builder: TestProcessorBuilder) -> TestProcessor;

    /// Drive the executor. Scenarios may call `recorder.mark("label")` to drop
    /// labeled markers into the trace for span-based validation later.
    fn drive(&mut self, proc: &mut TestProcessor, recorder: &TraceRecorder) -> DriveOutcome;

    /// Scenario-specific assertions on top of the universal invariants (which
    /// the harness already checks). Default: no-op.
    fn validate(&self, _trace: &ExecutorTrace, _outcome: &DriveOutcome) -> Result<(), String> {
        Ok(())
    }
}

use crate::executor::test_harness::invariants::{verify_universal_invariants, PostRunSnapshot};

/// Top-level test entry point. Given a scenario, builds the processor, drives
/// the executor, captures a post-run snapshot, verifies universal invariants,
/// then runs the scenario's own `validate()`.
pub fn run_executor_test<S: ExecutorTestScenario>(mut scenario: S) -> Result<(), String> {
    let recorder = TraceRecorder::default();
    let builder = TestProcessorBuilder::new(recorder.clone());
    let mut proc = scenario.configure(builder);

    let outcome = scenario.drive(&mut proc, &recorder);

    let snapshot = PostRunSnapshot {
        core_simulated: proc.core.time.simulated_time(),
        vcpu_simulated: proc.vcpus.iter().map(|v| v.time.simulated_time()).collect(),
    };

    // Drop the processor (and its embedded recorder clones held inside the
    // tracing decorators) so `finish()` can take ownership of the final Arc.
    drop(proc);

    let trace = recorder.finish();

    verify_universal_invariants(&trace, &snapshot, &outcome)
        .map_err(|e| format!("universal invariant violated: {e:?}"))?;

    scenario.validate(&trace, &outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{DefaultExecutor, ExecutorKind};

    #[test]
    fn drive_outcome_constructs() {
        let o = DriveOutcome {
            expected_total_cycles: 5000,
            cycle_tolerance: 0,
            expected_system_ticks: Some(5),
            extra: Box::new(()),
        };
        assert_eq!(o.expected_total_cycles, 5000);
        assert_eq!(o.expected_system_ticks, Some(5));
    }

    struct TrivialScenario;

    impl ExecutorTestScenario for TrivialScenario {
        fn configure(&mut self, builder: TestProcessorBuilder) -> TestProcessor {
            builder
                .with_executor(ExecutorKind::stride(DefaultExecutor::with_stride_length(
                    1000,
                )))
                .build()
        }

        fn drive(&mut self, p: &mut TestProcessor, _: &TraceRecorder) -> DriveOutcome {
            p.executor
                .begin(&mut p.vcpus, &mut p.core, &mut p.plugins, &5000_u64)
                .unwrap();
            DriveOutcome {
                expected_total_cycles: 5000,
                cycle_tolerance: 0,
                expected_system_ticks: Some(5),
                extra: Box::new(()),
            }
        }
    }

    #[test]
    fn run_executor_test_drives_and_validates() {
        run_executor_test(TrivialScenario).unwrap();
    }
}

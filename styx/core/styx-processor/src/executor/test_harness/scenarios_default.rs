// SPDX-License-Identifier: BSD-2-Clause
//! Reference [`ExecutorTestScenario`] implementations used by the crate's own
//! executor tests. Also serve as copy-paste templates for other crates'
//! executor tests (gdbserver, fuzzer, …).
//!
//! [`ExecutorTestScenario`]: crate::executor::test_harness::ExecutorTestScenario

use crate::executor::test::ScriptedParams;
use crate::executor::test_harness::{
    run_executor_test, DriveOutcome, ExecutorTestScenario, ExecutorTrace, TestProcessor,
    TestProcessorBuilder, TraceRecorder,
};
use crate::executor::{DefaultExecutor, ExecutorKind};

/// Asserts exact stride cadence for a [`DefaultExecutor`].
pub struct StrideScenario {
    pub stride: u64,
    pub total_instructions: u64,
}

impl ExecutorTestScenario for StrideScenario {
    fn configure(&mut self, builder: TestProcessorBuilder) -> TestProcessor {
        builder
            .with_executor(ExecutorKind::stride(DefaultExecutor::with_stride_length(
                self.stride,
            )))
            .build()
    }

    fn drive(&mut self, p: &mut TestProcessor, _: &TraceRecorder) -> DriveOutcome {
        p.executor
            .begin(
                &mut p.vcpus,
                &mut p.core,
                &mut p.plugins,
                &self.total_instructions,
            )
            .unwrap();
        DriveOutcome {
            expected_total_cycles: self.total_instructions,
            cycle_tolerance: 0,
            expected_system_ticks: Some((self.total_instructions / self.stride) as usize),
            extra: Box::new(()),
        }
    }

    fn validate(&self, trace: &ExecutorTrace, _: &DriveOutcome) -> Result<(), String> {
        println!(
            "validating stride scenario, expected stride: {0}",
            self.stride
        );

        // For each stride in traced system ticks, check if the delta simulated
        // time is the expected stride length.
        for (tick_idx, delta) in trace.system_ticks().enumerate() {
            let simulated_time = delta.simulated_time;
            println!("stride: {tick_idx}, simulated_time: {simulated_time}");
            if simulated_time != self.stride {
                return Err(format!(
                    "system tick {tick_idx} delta.simulated={} expected {}",
                    delta.simulated_time, self.stride
                ));
            }
        }
        Ok(())
    }
}

/// 2 vCPUs; vCPU 0 issues a configurable exit on a specified round. The run
/// halts after that round. Used to verify the harness correctly snapshots
/// partial-round state.
pub struct ScriptedExitScenario {
    pub stride: u64,
    pub params: ScriptedParams,
}

impl ExecutorTestScenario for ScriptedExitScenario {
    fn configure(&mut self, builder: TestProcessorBuilder) -> TestProcessor {
        use crate::cpu::DummyBackend;
        use crate::executor::test::ScriptedBackend;

        builder
            .with_vcpu_backend(Box::new(ScriptedBackend::new(self.params.clone())))
            .with_vcpu_backend(Box::new(DummyBackend))
            .with_executor(ExecutorKind::stride(DefaultExecutor::with_stride_length(
                self.stride,
            )))
            .build()
    }

    fn drive(&mut self, p: &mut TestProcessor, _: &TraceRecorder) -> DriveOutcome {
        // Run for exactly exit_on_round * stride cycles
        let total_constraint = self.stride * u64::from(self.params.exit_on_round);
        p.executor
            .begin(&mut p.vcpus, &mut p.core, &mut p.plugins, &total_constraint)
            .unwrap();
        DriveOutcome {
            expected_total_cycles: total_constraint,
            cycle_tolerance: 0,
            expected_system_ticks: Some(self.params.exit_on_round as usize),
            extra: Box::new(()),
        }
    }

    /// Pins down the exit-round semantics that the universal invariants
    /// deliberately tolerate:
    /// - vcpu 0 (scripted) ticks `exit_on_round - 1` times. It record_strides
    ///   on the exit round but post_stride_processing is skipped.
    /// - vcpu 1 (dummy) ticks `exit_on_round` times. Every round including
    ///   the exit round.
    /// - Every system tick is exactly `stride` wide.
    fn validate(&self, trace: &ExecutorTrace, _: &DriveOutcome) -> Result<(), String> {
        let exit_round = self.params.exit_on_round as usize;
        let expected_vcpu0 = exit_round.saturating_sub(1);

        let vcpu0_ticks = trace.vcpu_ticks(0).count();
        println!("vcpu 0 ticked {vcpu0_ticks}");
        if vcpu0_ticks != expected_vcpu0 {
            return Err(format!(
                "vcpu 0 ticked {vcpu0_ticks} times, expected {expected_vcpu0} \
                 (post_stride_processing should be skipped on exit round)"
            ));
        }

        let vcpu1_ticks = trace.vcpu_ticks(1).count();
        println!("vcpu 1 ticked {vcpu1_ticks}");
        if vcpu1_ticks != exit_round {
            return Err(format!(
                "vcpu 1 ticked {vcpu1_ticks} times, expected {exit_round} \
                 (non-exiting vcpu should tick every round)"
            ));
        }

        for (tick_idx, delta) in trace.system_ticks().enumerate() {
            if delta.simulated_time != self.stride {
                return Err(format!(
                    "system tick {tick_idx} delta.simulated={} expected {}",
                    delta.simulated_time, self.stride
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stride_scenario_default_executor() {
        run_executor_test(StrideScenario {
            stride: 1000,
            total_instructions: 5000,
        })
        .unwrap();
    }

    #[test]
    fn stride_scenario_single_step_like() {
        // stride = 1 means 5 ticks for 5 instructions.
        run_executor_test(StrideScenario {
            stride: 1,
            total_instructions: 5,
        })
        .unwrap();
    }
}

#[cfg(test)]
mod scripted_tests {
    use super::*;
    use styx_cpu_type::TargetExitReason;

    #[test]
    fn scripted_bus_error_on_round_2() {
        run_executor_test(ScriptedExitScenario {
            stride: 1000,
            params: ScriptedParams {
                exit_on_round: 2,
                exit_reason: TargetExitReason::BusError,
                partial_count: None,
            },
        })
        .unwrap();
    }

    #[test]
    fn scripted_host_stop_on_round_3() {
        run_executor_test(ScriptedExitScenario {
            stride: 1000,
            params: ScriptedParams {
                exit_on_round: 3,
                exit_reason: TargetExitReason::HostStopRequest,
                partial_count: None,
            },
        })
        .unwrap();
    }

    #[test]
    fn scripted_host_stop_mid_stride_non_fatal() {
        run_executor_test(ScriptedExitScenario {
            stride: 1000,
            params: ScriptedParams {
                exit_on_round: 3,
                exit_reason: TargetExitReason::HostStopRequest,
                partial_count: Some(500),
            },
        })
        .unwrap();
    }

    #[test]
    fn scripted_host_stop_mid_stride_fatal() {
        run_executor_test(ScriptedExitScenario {
            stride: 1000,
            params: ScriptedParams {
                exit_on_round: 3,
                exit_reason: TargetExitReason::IllegalInstruction,
                partial_count: Some(500),
            },
        })
        .unwrap();
    }
}

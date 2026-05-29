// SPDX-License-Identifier: BSD-2-Clause
//! Executor correctness test harness.
//!
//! Provides a reusable scaffold for verifying any [`StrideExecutor`] or
//! [`CustomExecutor`] implementation upholds round-level accounting invariants:
//! `core.time.advance` is consistent with `simulated_time()`, per-vCPU
//! `record_stride` is consistent with each vCPU's `simulated_time()`, and
//! `post_stride_processing` runs for every live vCPU between event
//! distributor ticks.
//!
//! Scenarios are plugged in via [`ExecutorTestScenario`]. Call
//! [`run_executor_test`] to exercise a scenario and validate invariants.
//!
//! See the `executor/test_harness/scenarios_default.rs` for reference
//! scenario implementations.
//!
//! [`StrideExecutor`]: super::StrideExecutor
//! [`CustomExecutor`]: super::CustomExecutor

mod builder;
mod decorators;
mod invariants;
mod scenario;
mod trace;

#[cfg(test)]
pub mod scenarios_default;

pub use builder::{TestProcessor, TestProcessorBuilder};
pub use decorators::{TracingPrimaryEc, TracingVcpuEc};
pub use invariants::{verify_universal_invariants, InvariantViolation, PostRunSnapshot};
pub use scenario::{run_executor_test, DriveOutcome, ExecutorTestScenario};
pub use trace::{ExecutorEvent, ExecutorTrace, ExecutorTraceSpan, TraceEntry, TraceRecorder};

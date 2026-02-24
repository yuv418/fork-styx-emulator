// SPDX-License-Identifier: BSD-2-Clause
//! Test processor builder with tracing wrappers pre-installed.

use std::sync::Arc;

use crate::core::{ProcessorCore, VcpuCore};
use crate::cpu::{CpuBackend, DummyBackend};
use crate::event_controller::{
    DummyEventController, DummyEventDistributor, EventController, EventControllerImpl,
    EventDistributor, EventDistributorImpl,
};
use crate::executor::test_harness::decorators::{TracingPrimaryEc, TracingVcpuEc};
use crate::executor::test_harness::trace::TraceRecorder;
use crate::executor::time::{ProcessorTime, VcpuTime};
use crate::executor::{Executor, ExecutorKind};
use crate::memory::physical::MemoryBackend;
use crate::memory::Mmu;
use crate::plugins::collection::Plugins;

/// A minimal processor wired up for executor testing, with tracing decorators
/// already installed around the primary and per-vCPU event controllers.
pub struct TestProcessor {
    pub vcpus: Vec<VcpuCore>,
    pub core: ProcessorCore,
    pub plugins: Plugins,
    pub executor: Executor,
}

/// Fluent builder for a [`TestProcessor`].
///
/// Defaults:
/// - 1 vCPU running [`DummyBackend`]
/// - [`DummyEventDistributor`] as the primary EC impl
/// - No plugins
/// - Executor **must** be set via [`TestProcessorBuilder::with_executor`].
pub struct TestProcessorBuilder {
    recorder: TraceRecorder,
    vcpu_backends: Vec<Box<dyn CpuBackend>>,
    vcpu_ec_impls: Vec<Box<dyn EventControllerImpl>>,
    primary_impl: Box<dyn EventDistributorImpl>,
    executor: Option<ExecutorKind>,
}

impl TestProcessorBuilder {
    pub fn new(recorder: TraceRecorder) -> Self {
        Self {
            recorder,
            vcpu_backends: Vec::new(),
            vcpu_ec_impls: Vec::new(),
            primary_impl: Box::new(DummyEventDistributor::default()),
            executor: None,
        }
    }

    /// Configure N vCPUs each using the default [`DummyBackend`] and a default
    /// per-vCPU event-controller impl. For custom backends call
    /// [`TestProcessorBuilder::with_vcpu_backend`] and
    /// [`TestProcessorBuilder::with_vcpu_ec_impl`] in tandem.
    pub fn with_vcpus(mut self, count: usize) -> Self {
        self.vcpu_backends.clear();
        self.vcpu_ec_impls.clear();
        for _ in 0..count {
            self.vcpu_backends.push(Box::new(DummyBackend));
            self.vcpu_ec_impls
                .push(Box::new(DummyEventController::default()));
        }
        self
    }

    /// Append a vCPU with the given CPU backend. Uses a default per-vCPU event
    /// controller impl; pair with [`TestProcessorBuilder::with_vcpu_ec_impl`]
    /// if you need a custom one.
    pub fn with_vcpu_backend(mut self, backend: Box<dyn CpuBackend>) -> Self {
        self.vcpu_backends.push(backend);
        self.vcpu_ec_impls
            .push(Box::new(DummyEventController::default()));
        self
    }

    /// Replace the event-controller impl for the most recently added vCPU.
    pub fn with_vcpu_ec_impl(mut self, impl_: Box<dyn EventControllerImpl>) -> Self {
        *self.vcpu_ec_impls.last_mut().expect(
            "with_vcpu_ec_impl requires a vcpu added first (via with_vcpu_backend or with_vcpus)",
        ) = impl_;
        self
    }

    pub fn with_primary_ec_impl(mut self, impl_: Box<dyn EventDistributorImpl>) -> Self {
        self.primary_impl = impl_;
        self
    }

    pub fn with_executor(mut self, exec: ExecutorKind) -> Self {
        self.executor = Some(exec);
        self
    }

    pub fn build(mut self) -> TestProcessor {
        // Default: 1 vCPU if nothing configured.
        if self.vcpu_backends.is_empty() {
            self.vcpu_backends.push(Box::new(DummyBackend));
            self.vcpu_ec_impls
                .push(Box::new(DummyEventController::default()));
        }
        assert_eq!(self.vcpu_backends.len(), self.vcpu_ec_impls.len());

        // Wrap primary EC impl.
        let tracing_primary: Box<dyn EventDistributorImpl> = Box::new(TracingPrimaryEc::new(
            self.primary_impl,
            self.recorder.clone(),
        ));
        let primary = EventDistributor::new(tracing_primary);

        // Wrap each per-vCPU EC impl.
        let vcpus: Vec<VcpuCore> = self
            .vcpu_backends
            .into_iter()
            .zip(self.vcpu_ec_impls)
            .enumerate()
            .map(|(idx, (cpu, ec_impl))| {
                let tracing_ec: Box<dyn EventControllerImpl> =
                    Box::new(TracingVcpuEc::new(ec_impl, idx, self.recorder.clone()));
                VcpuCore {
                    cpu,
                    mmu: Mmu::default(),
                    event_controller: EventController::new(
                        tracing_ec,
                        idx.try_into().expect("too many vcpus"),
                    ),
                    time: VcpuTime::default(),
                }
            })
            .collect();

        let core = ProcessorCore {
            memory: Arc::new(MemoryBackend::default()),
            event_controller: primary,
            time: ProcessorTime::default(),
        };

        let plugins = Plugins { plugins: vec![] };

        let executor_kind = self
            .executor
            .expect("TestProcessorBuilder::build requires with_executor");
        let executor = Executor::new(executor_kind);

        TestProcessor {
            vcpus,
            core,
            plugins,
            executor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::test_harness::trace::TraceRecorder;
    use crate::executor::{DefaultExecutor, ExecutorKind};

    #[test]
    fn default_builder_produces_single_vcpu_processor() {
        let rec = TraceRecorder::default();
        let tp = TestProcessorBuilder::new(rec)
            .with_executor(ExecutorKind::stride(DefaultExecutor::with_stride_length(
                1000,
            )))
            .build();
        assert_eq!(tp.vcpus.len(), 1);
    }

    #[test]
    fn with_vcpus_sets_count() {
        let rec = TraceRecorder::default();
        let tp = TestProcessorBuilder::new(rec)
            .with_vcpus(3)
            .with_executor(ExecutorKind::stride(DefaultExecutor::with_stride_length(
                1000,
            )))
            .build();
        assert_eq!(tp.vcpus.len(), 3);
    }
}

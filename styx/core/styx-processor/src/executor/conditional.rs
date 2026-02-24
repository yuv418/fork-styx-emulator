// SPDX-License-Identifier: BSD-2-Clause

use std::sync::Arc;

use styx_cpu_type::TargetExitReason;
use styx_errors::UnknownError;

use crate::{executor::Delta, processor::core_configs::ConfigRequestedStrideLength};

use super::{HaltFn, StrideExecutor};

/// Executor that stops when a custom function returns true.
///
/// Otherwise behavior is identical to the [`super::DefaultExecutor`].
///
/// Notably implements [StrideExecutor].
pub struct ConditionalExecutor<F> {
    should_stop: Arc<F>,
    stride_length: u64,
}

impl<F> ConditionalExecutor<F> {
    /// Construct [ConditionalExecutor] with custom stop function.
    ///
    /// `should_stop` function is called every stride to determine if the
    /// processor should continue executing.
    pub fn new(should_stop: F) -> Self {
        Self {
            should_stop: Arc::new(should_stop),
            stride_length: 1000,
        }
    }
}

impl<F: Fn() -> bool + 'static + Send + Sync> StrideExecutor for ConditionalExecutor<F> {
    fn get_stride_length(&self) -> u64 {
        self.stride_length
    }

    fn halt_emulation(&mut self) -> Option<HaltFn> {
        let should_stop = self.should_stop.clone();
        Some(Box::new(
            move |reason: &TargetExitReason, _delta: &Delta| reason.fatal() || should_stop(),
        ))
    }

    fn init(
        &mut self,
        _proc: &mut crate::processor::BuildingProcessor,
    ) -> Result<(), UnknownError> {
        self.stride_length = _proc
            .config
            .get_or_default::<ConfigRequestedStrideLength>()
            .preferred_stride_length;
        Ok(())
    }
}

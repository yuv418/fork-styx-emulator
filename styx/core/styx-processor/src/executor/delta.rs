// SPDX-License-Identifier: BSD-2-Clause
/// Reports metrics after a stride of emulation.
#[derive(Debug, Clone, Default)]
pub struct Delta {
    /// Elapsed wall clock time since last tick.
    ///
    /// This is the real-world duration of the stride, not a processor-specific or simulated time.
    pub time: std::time::Duration,
    /// Number of instructions executed.
    pub count: u64,
}

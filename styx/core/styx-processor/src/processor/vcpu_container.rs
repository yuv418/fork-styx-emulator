// SPDX-License-Identifier: BSD-2-Clause
use std::ops::{Deref, DerefMut};

use crate::core::VcpuId;

const TYPICAL_VCPUS: usize = 16;
#[derive(Clone, Debug, Default)]
pub struct VcpuContainer<T>(smallvec::SmallVec<[T; TYPICAL_VCPUS]>);

impl<T> VcpuContainer<T> {
    pub fn get(&self, idx: VcpuId) -> Option<&T> {
        self.0.get(idx as usize)
    }

    pub fn enumerate_mut(&mut self) -> impl Iterator<Item = (VcpuId, &mut T)> {
        self.iter_mut()
            .enumerate()
            .map(|(idx, t)| (idx as VcpuId, t))
    }

    pub fn push(&mut self, item: T) {
        self.0.push(item)
    }
}

impl<T> Deref for VcpuContainer<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.0.deref()
    }
}
impl<T> DerefMut for VcpuContainer<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.deref_mut()
    }
}

impl<V> FromIterator<V> for VcpuContainer<V> {
    fn from_iter<T: IntoIterator<Item = V>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

// SPDX-License-Identifier: BSD-2-Clause
// Assisted-By: claude-opus-4.5[1m]
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use log::info;

#[derive(Debug, Default, Clone, Copy)]
pub struct ProfileEntry {
    pub count: u64,
    pub total_ns: u128,
    pub min_ns: u128,
    pub max_ns: u128,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Profiler {
    min_address: u64,
    max_address: u64,
    entries: BTreeMap<u64, ProfileEntry>,
    in_flight: Option<(u64, Instant)>,
    profiling: bool,
    path: String,
    total_elapsed: u128,
}

#[allow(dead_code)]
impl Profiler {
    pub fn new(min_address: u64, max_address: u64, path: String) -> Self {
        info!("profiler start");
        Self {
            min_address,
            max_address,
            entries: BTreeMap::new(),
            in_flight: None,
            profiling: false,
            path,
            total_elapsed: 0,
        }
    }

    pub fn start_instruction_profile(&mut self, address: u64) {
        if address == self.min_address {
            info!("starting profiling");
            self.profiling = true;
        }
        if self.profiling {
            self.in_flight = Some((address, Instant::now()));
        }
        if address == self.max_address {
            info!("stopping profiling");
            self.profiling = false;
        }
    }

    pub fn end_instruction_profile(&mut self) {
        let last_profile = self.in_flight.is_some() && !self.profiling;

        if self.in_flight.is_none() && !self.profiling {
            return;
        }

        // info!("finish profiling");
        let now = Instant::now();
        let (address, start) = self.in_flight.take().unwrap();
        let elapsed = now.duration_since(start).as_nanos();
        /*info!(
            "self.entries len {} is now {:x?}",
            self.entries.len(),
            self.entries
        );*/
        let entry = self.entries.entry(address).or_default();
        if entry.count == 0 {
            entry.min_ns = elapsed;
            entry.max_ns = elapsed;
        } else {
            entry.min_ns = entry.min_ns.min(elapsed);
            entry.max_ns = entry.max_ns.max(elapsed);
        }
        entry.count += 1;
        entry.total_ns += elapsed;
        self.total_elapsed += elapsed;

        if last_profile {
            info!("export profiling");
            info!("total elapsed was {} ns", self.total_elapsed);
            self.export()
                .expect("Couldn't export profiling information");
        }
    }

    fn export(&self) -> std::io::Result<()> {
        let mut out = BufWriter::new(File::create(PathBuf::from(&self.path))?);
        writeln!(out, "address,count,total_ns,mean_ns,min_ns,max_ns")?;
        for (address, entry) in &self.entries {
            writeln!(
                out,
                "{:#x},{},{},{},{},{}",
                address,
                entry.count,
                entry.total_ns,
                entry.total_ns / entry.count as u128,
                entry.min_ns,
                entry.max_ns,
            )?;
        }
        out.flush()
    }
}

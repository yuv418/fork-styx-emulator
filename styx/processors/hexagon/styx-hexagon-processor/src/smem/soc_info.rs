// SPDX-License-Identifier: BSD-2-Clause

// From drivers/soc/qcom/socinfo.c in the Linux kernel.

use bytemuck::{bytes_of, NoUninit};
use styx_core::memory::Mmu;

use super::Smem;

const SMEM_SOCINFO_BUILD_ID_LENGTH: usize = 32;
const SMEM_SOCINFO_CHIP_ID_LENGTH: usize = 32;

#[repr(packed)]
#[derive(NoUninit, Clone, Copy, Default)]
struct QcomSocInfo {
    fmt: u32,
    id: u32,
    ver: u32,
    build_id: [u8; SMEM_SOCINFO_BUILD_ID_LENGTH],
    /* Version 2 */
    raw_id: u32,
    raw_ver: u32,
    /* Version 3 */
    hw_plat: u32,
    /* Version 4 */
    plat_ver: u32,
    /* Version 5 */
    accessory_chip: u32,
    /* Version 6 */
    hw_plat_subtype: u32,
    /* Version 7 */
    pmic_model: u32,
    pmic_die_rev: u32,
    /* Version 8 */
    pmic_model_1: u32,
    pmic_die_rev_1: u32,
    pmic_model_2: u32,
    pmic_die_rev_2: u32,
    /* Version 9 */
    foundry_id: u32,
    /* Version 10 */
    serial_num: u32,
    /* Version 11 */
    num_pmics: u32,
    pmic_array_offset: u32,
    /* Version 12 */
    chip_family: u32,
    raw_device_family: u32,
    raw_device_num: u32,
    /* Version 13 */
    nproduct_id: u32,
    chip_id: [u8; SMEM_SOCINFO_CHIP_ID_LENGTH],
    /* Version 14 */
    num_clusters: u32,
    ncluster_array_offset: u32,
    num_defective_parts: u32,
    ndefective_parts_array_offset: u32,
    /* Version 15 */
    nmodem_supported: u32,
}

pub(crate) fn write_socinfo(socinfo_addr: u32, mmu: &mut Mmu) {
    let socinfo = QcomSocInfo {
        fmt: 0,
        ..Default::default()
    };

    Smem::write_smem_struct(socinfo_addr, &socinfo, mmu);
}

/* SPDX-License-Identifier: BSD-2-Clause */
/* Unified two-core PowerPC 405 firmware for the GDB multicore example.
 *
 * ONE position-independent routine drives every test and the interactive demo.
 * The identical .text bytes run on both cores at different base addresses; all
 * per-core differences are supplied by the builder through registers:
 *
 *   r3 = this core's private counter base (PRIV_BASE_0 / PRIV_BASE_1)
 *   r1 = this core's stack top            (STACK_BASE_0 / STACK_BASE_1)
 *   PC = this core's copy of the code     (CODE_BASE_0  / CODE_BASE_1)
 *
 * Because the stack pointer arrives in r1 (rather than being loaded with an
 * absolute `lis`), each core gets its own isolated stack and the bytes stay
 * position-independent -- this is the key change that let the old single-core
 * `call.S` merge with the multi-core `loop.S`.
 *
 * The loop calls two subroutines with `bl` (PowerPC branch-and-link, the
 * canonical "call"):
 *   incr_private -- read-modify-writes THIS core's private word at 0(r3).
 *   incr_shared  -- read-modify-writes the SHARED word at SHARED_COUNTER.
 *
 * The gdb `nexti`/`stepi` tests step over `bl incr_private`, NOT
 * `bl incr_shared`: incr_private touches a per-core word, so stepping over it
 * advances exactly one core's counter by one -- a deterministic assertion.
 * Stepping over incr_shared could not be checked per-core, because either core
 * may have advanced the shared counter (it is a race by design).
 *
 * Both subroutines follow the PowerPC SysV stack conventions (NULL back-chain
 * terminator + `stwu`-allocated frames) so the gdb client can unwind the call
 * stack -- that unwind is what lets `nexti` recognise the `bl` as a call.
 *
 * The shared RMW is intentionally non-atomic (lwz/addi/stw): the pcode/SLEIGH
 * PPC model does not implement lwarx/stwcx. reservations, so an atomic loop
 * would spin forever on this backend.
 *
 * Assembled at build time by ../build.rs; its `.text` is embedded as FIRMWARE
 * and its symbol object is loaded by the nexti tests (FIRMWARE_OBJ_FILE). The
 * `firmware_layout_invariants` test pins the offsets the example depends on.
 * `make rust`/`make dis` (see Makefile) inspect the encoding. */
    .section .text
    .globl _start
_start:                          /* 0x00 */
    li     0, 0                  /* r0 = 0 (first insn is a simple non-branch so   */
                                 /*         stepi from entry lands at base+4)      */
    stw    0, 0(1)               /* 0x04: NULL back-chain terminator at stack top  */
    stwu   1, -32(1)             /* 0x08: allocate _start frame (SysV)             */
    lis    5, 0x0003             /* 0x0c: r5 = SHARED_COUNTER (0x0003_0000) base    */
1:                               /* numeric local label: emits NO symbol, so the   */
                                 /* `bl` below stays inside the `_start` function   */
                                 /* symbol. gdb's PPC prologue analyzer then sees   */
                                 /* _start's `stwu` frame setup ahead of the call,  */
                                 /* gives caller and callee distinct frame CFAs,    */
                                 /* and recognises the `bl` as a call -- which is    */
                                 /* what lets `nexti` step OVER it. A named label    */
                                 /* here would split off a frame-less function and  */
                                 /* degrade `nexti` to `stepi`.                     */
    bl     incr_private          /* 0x10: CALL -- nexti/stepi test target          */
    nop                          /* 0x14: bl return target; nexti must land here   */
    bl     incr_shared           /* 0x18: CALL -- shared counter RMW               */
    b      1b                    /* 0x1c: spin                                     */
incr_private:                    /* 0x20: stepi over the bl must land here         */
    mflr   0                     /* 0x20: standard SysV prologue (stwu/stw follow) */
    stwu   1, -16(1)
    stw    0, 20(1)
    lwz    4, 0(3)               /* 0x2c: r4 = *priv  (per-core, via r3)           */
    addi   4, 4, 1               /* 0x30: priv++                                   */
    stw    4, 0(3)               /* 0x34: *priv = r4  (PRIV_STW_OFFSET)            */
    lwz    0, 20(1)
    mtlr   0
    addi   1, 1, 16
    blr                          /* 0x44: return to call+4 (the nop)               */
incr_shared:                     /* 0x48                                           */
    mflr   0
    stwu   1, -16(1)
    stw    0, 20(1)
    lwz    6, 0(5)               /* r6 = *shared (non-atomic RMW)                  */
    addi   6, 6, 1
    stw    6, 0(5)               /* *shared = r6                                   */
    lwz    0, 20(1)
    mtlr   0
    addi   1, 1, 16
    blr                          /* 0x6c: return -- firmware ends at 0x70 (112 B)  */

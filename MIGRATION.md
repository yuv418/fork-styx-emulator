# Migration Guide

## Multi-Processor Changes
This release reworks `styx-processor` to support **multiple vCPUs per
processor**. The change is broad but mostly mechanical. It groups into three concerns:

1. **[Multi-vCPU](#1-multi-vcpu)**: the processor core split, the `ProcessorBundle` builder, and the per-vCPU access pattern (`proc.vcpus[..]`).
2. **[Event distributor](#2-event-distributor)**: the event-controller trait split and the reshaped `Peripheral::tick`.
3. **[Timing](#3-timing-delta-vs-globaldelta)**: the new `Delta` vs `GlobalDelta` distinction.

### The core split
The old single `ProcessorCore` (which bundled cpu + mmu + event_controller) is split in two:

| Type | Scope | Holds |
|---|---|---|
| `ProcessorCore` | processor-wide, shared between vCPUs | `Arc<MemoryBackend>`, `EventDistributor`, `ProcessorTime` |
| `VcpuCore` | per-vCPU | `Box<dyn CpuBackend>`, `Mmu`, (secondary) `EventController`, `VcpuTime` |

A `Processor` now exposes `pub vcpus: Vec<VcpuCore>` and `pub core: ProcessorCore`.
Single-vCPU processors simply have one entry in `vcpus`.

## 1. Multi-vCPU
### Building a processor: `ProcessorBundle::builder()`
Creating a `ProcessorBundle` was already a bit annoying, with multiple vcpus it is even more annoying. The new `ProcessorBundleBuilder` and VcpuBundleBuilders provide sensible defaults for most processor/vCPU components and has methods to add vCPUs.

```rust
// OLD
Ok(ProcessorBundle {
    cpu: Box::new(MyCpu),
    tlb: Box::new(MyTlb),
    event_controller: Box::new(MyEc),
    memory,
    peripherals,
    loader_hints,
})

// NEW: one vCPU, configured through the nested VcpuBundleBuilder
Ok(ProcessorBundle::builder()
    .with_memory(memory)
    .with_vcpu(|v| v.with_cpu(MyCpu).with_tlb(MyTlb).with_event_controller(MyEc))
    .add_peripheral(MyUart::new())
    .with_arch_hint(Arch::Arm)
    .build()?)

// NEW: 16 vCPUs configured quickly via `with_vcpus`
Ok(ProcessorBundle::builder()
    .with_memory(memory)
    .with_vcpus(16, |_idx, v| v.with_cpu(DummyBackend).with_event_controller(MyEc))
    .add_peripheral(MyUart::new())
    .with_arch_hint(Arch::Arm)
    .build()?)
```

Reference the docs for `ProcessorBundleBuilder` for all the available methods.

One additional method to note on the `ProcessorBundleBuilder` is `modify_memory()` with gives mutable access to the `MemoryBackend` allowing you to map memory from within the builder.

```rust
.with_memory(MemoryBackend::new_region_store())
.modify_memory(|mem| { mem.add_memory_region(/* .. */)?; Ok(()) })?
```

### Per-vCPU access in `init()` and elsewhere
`proc.core.cpu` / `proc.core.mmu` no longer exist. In
`ProcessorImpl::init` and other inits, `BuildingProcessor` now exposes this via `vcpus`:

```rust
pub struct BuildingProcessor<'a> {
    pub vcpus: &'a mut [VcpuCore],   // NEW
    pub core: &'a mut ProcessorCore,
    pub runtime: &'a mut ProcessorRuntime,
    pub routes: RoutesBuilder,
    // ..
}
```

Replace `proc.core.cpu` → `proc.vcpus[0].cpu`, `proc.core.mmu` → `proc.vcpus[0].mmu` for previous single-vCPU processors. Multi-vCPU systems can add hooks to one or many vCPUs.

Also consider using `proc.memory()` which gives a reference to the `MemoryBackend` if your memory operations are on physical addresses.

### Running processors: `Processor::run()` vs `Processor::run_multi()`

| Method | Returns | Notes |
|---|---|---|
| `proc.run(bounds)` | `EmulationReport` | **errors if there is more than one vCPU** |
| `proc.run_multi(bounds)` | `Vec<EmulationReport>` | one report per vCPU |

Single-vCPU callers are unaffected. Multi-vCPU callers must use `run_multi`.

### Other surface changes

- **`CoreHandle::vcpu_id() -> VcpuId`** identifies the current vCPU.
- **`Processor::memory() -> &Arc<MemoryBackend>`** for shared physical memory.
- **`Processor::for_vcpu(|v| ..)`** and **`Processor::add_hooks(|vcpu_id| StyxHook)`** apply an operation/hook across every vCPU.

### Executor changes

The old `ExecutorImpl` trait is gone, replaced by two traits behind an
`ExecutorKind`:

- **`StrideExecutor`**: the common case. The Styx core drives the multi-vCPU
  loop; you only supply `get_stride_length()`, `halt_emulation() ->
  Option<HaltFn>` plus the `init`/`emulation_setup`/`emulation_teardown`/`tick`
  lifecycle hooks.
- **`CustomExecutor`**: full control for debuggers/fuzzers. One
  method: `execute(&mut [VcpuCore], &mut ProcessorCore, &mut Plugins,
  &ExecutionConstraintConcrete) -> Result<Vec<EmulationReport>>`. A custom
  executor is responsible for ticking all components itself.

## 2. Event distributor

The single `EventControllerImpl` trait is split into two.

**For single-vCPU processors the split is easy:** your existing controller
logic stays as the secondary event controller and the Styx core has a
`SingleVcpuEventController` that performs as the event distributor.

| Trait | Scope | Role |
|---|---|---|
| `EventControllerImpl` (secondary) | per-vCPU | latch/next/execute interrupts on *its* CPU, also routes from vCPU -> Event Distributor |
| `EventDistributorImpl` (primary) | processor-wide | owns peripherals and **routes** IRQs raised by peripheral ticks to the correct vCPU |

### Per-vCPU `EventControllerImpl`
Migrating the per-vCPU event controller should be simple.

Keep all your latch/priority/ISR logic. Only two signatures move:

- `next()` **no longer takes `&mut Peripherals`** (peripherals moved to the event distributor).
- `tick()` **gains a `&Delta`** parameter.

`latch()`, `execute()`, `finish_interrupt()`, `reset()`, `init()` are unchanged.
The dummy is still `DummyEventController`. `EventController::new()` now takes
`(Box<dyn EventControllerImpl>, vcpu_index: VcpuId)`, and `finish_interrupt()`
now returns `Option<ExceptionNumber>`.

### `EventDistributorImpl`
```rust
pub trait EventDistributorImpl {
    fn init(&mut self, vcpus: &mut [VcpuCore], memory: &Arc<MemoryBackend>) -> Result<(), UnknownError> { Ok(()) }
    fn on_processor_start(&mut self, vcpus: &mut [VcpuCore]) -> Result<(), UnknownError> { Ok(()) }
    fn on_processor_stop(&mut self, vcpus: &mut [VcpuCore]) -> Result<(), UnknownError> { Ok(()) }
    fn tick(&mut self, delta: &GlobalDelta, pending_irqs: &[ExceptionNumber], vcpus: &mut [VcpuCore]) -> Result<(), UnknownError> { Ok(()) }
    fn reset(&mut self, cpu: &mut dyn CpuBackend, mmu: &mut Mmu) -> Result<(), UnknownError> { Ok(()) }
    // latch(..), etc.
}
```

- **`SingleVcpuEventController`**
  routes every IRQ raised by a peripheral tick to `vcpus[0]`'s secondary
  controller. This is what almost every single-vCPU processor with
  interrupt-driven peripherals should use.
- **`DummyEventDistributor`** (the default): its `tick` **drops
  `pending_irqs`** (warning if any were pending). Nothing gets routed.

Write a custom `EventDistributorImpl` when you have genuine processor-wide
lifecycle/routing logic (e.g. routing IRQs to a *specific* core).

### `Peripheral::tick` has new signature and returns IRQs

Peripherals are now owned by the event distributor, which ticks
each one per round, collects the returned IRQs, and hands them to
`EventDistributorImpl::tick` for routing.

```rust
// OLD: latched directly onto the event controller
fn tick(&mut self, cpu: &mut dyn CpuBackend, mmu: &mut Mmu,
        ec: &mut dyn EventControllerImpl, delta: &Delta) -> Result<(), UnknownError> {
    if self.has_data() { ec.latch(self.irqn)?; }
    Ok(())
}

// NEW: returns the IRQs it wants raised
fn tick(&mut self, ctx: &PeripheralTickCtx<'_>) -> Result<RaisedIrqs, UnknownError> {
    let mut raised = RaisedIrqs::none();
    if self.has_data() { raised.push(self.irqn); }
    Ok(raised)
}
```

What `tick` can/can't do now:

- **Can:** read `ctx.delta` and read/write **physical** memory via `ctx.memory`
  (`&MemoryBackend`). This lets DMA-style peripherals write guest memory
  directly instead of staging through an MMIO hook.
- **Can't:** touch CPU registers, per-vCPU MMU/virtual addresses, or call
  `latch` directly. If you need register or virtual-memory access, register a
  hook in `init()`.

### Peripheral Hooks
Peripherals no longer live near vCPUs so hooks no longer
have access to the peripheral that owns them (previously via
`proc.event_controller.peripherals.get_expect::<MyPeripheral>()?;`).

The accepted way to share peripheral data with hooks is via an Arc'd data store.
See the stm32f107 processor's i2c implementation for an example.

```rust
// in Peripheral::init
cpu.add_hook(StyxHook::memory_write(
    base_addr + I2C_CR1_OFFSET,
    hooks::I2cCr1WHook { inner: i2c.clone() },
))?;

pub(crate) struct I2cCr1WHook {
    pub(crate) inner: Arc<Mutex<I2CPortInner>>,
}

impl MemoryWriteHook for I2cCr1WHook {
    fn call(
        &mut self,
        proc: CoreHandle,
        address: u64,
        _size: u32,
        data: &[u8],
    ) -> Result<(), UnknownError> {
        let port = self.inner.lock().unwrap();
        // do some stuff with `port`
        Ok(())
    }
}
```

## 3. Timing: `Delta` vs `GlobalDelta`

The split introduced **two delta types corresponding to processor vs vCPU time**.

| Type | Fields | Used by |
|---|---|---|
| `Delta` (per-vCPU, per-stride) | `count: u64`, `time: Duration` | secondary `EventControllerImpl::tick`, `post_stride_processing`, `HaltFn` |
| `GlobalDelta` (system-level, per-round) | `simulated_time: u64`, `wall_time: Duration` | `Peripheral::tick` (`ctx.delta`), `EventDistributorImpl::tick`, `Plugin::tick` |


Time accounting is tracked by `ProcessorCore::time` (`ProcessorTime`, the
processor-wide simulated clock, advanced once per round) and `VcpuCore::time`
(`VcpuTime`, per-vCPU). See the `styx_core::executor::time` module docs for the
full model.

## 1.0.0 to 1.2.0

### Emulation API Changes

The API for `Executor`, `CpuBackend` and `Processor` emulation has been updated to provide more detailed execution information. The return type for emulation methods has changed from `Result<TargetExitReason, ...>` to `Result<EmulationReport, ...>`.

#### Before

```rust
let exit_reason: TargetExitReason = processor.run(constraint)?;
// exit_reason is a TargetExitReason enum
```

#### After

```rust
let report: EmulationReport = processor.run(constraint)?;
// report is an EmulationReport containing:
// - exit_reason: TargetExitReason
// - instruction_count: u64
// - etc.
```

This change allows users to access additional execution information like instruction counts and other metrics without requiring separate API calls.

**NOTE**: this also propagated to the python and C bindings accordingly

## 0.53.0 to 1.0.0

At a very high level, this refactor reorganized the major processor components and improved interactions between them.  See (TODO: add link to diagram) to get an idea of how components now fit together.  

A major paradigm shift was moving from `Arc<Mutex<>>` based components to `mut` components.  We realized that we were both spending a lot of time in locks and that most of these locks were not really necessary, so we changed it.  For users, this mostly affects Styx API calls in minor ways

Most of the changes that average users will encounter have to do with defining and building a processor.  This document gives examples of how code was structured before and after to help users migrate to the new release.

Other notable changes that users might encounter includes changes to import paths.

By and large it is encouraged to use prelude imports from styx_core::prelude::*or styx_emulator::prelude::*; when possible. Additionally many of the common modules that were used were elevated to more ergonomic positions in the import (notably styx_core::sync::sync is now just styx_core::sync).

Inside of `styx_core`, a lot has changed as we transition to a slightly different internal crate structure. This is leading to a partial sunsetting of styx-cpu retaining the majority of backend functionality and a reduced need for a large number of crates, consolidating a lot of the processor-level logic into `styx-processor`.

### Processor Definition

The previous way of defining a new processor involved lots of duplicated, boiler-plate code that could just be copy-pasted from an existing definition.  We realized that the `ProcessorImpl` was entirely stateless and pretty much only performed initialization duties.  We combined the previous `ProcessorImpl` and `BuildableProcessor` traits into a single, simplified trait and moved other behavior to different parts of the codebase.

#### Before

```rust
pub struct ExampleCpu {
    cpu: CpuBackend,
    #[derivative(Debug = "ignore")]
    event_controller: Arc<EvtController>,
    weak_ref: Weak<Self>,
}

impl BuildableProcessor for ExampleCpu {
    fn from_builder(
        variant: impl Into<styx_core::cpu::arch::backends::ArchVariant>,
        endian: styx_core::cpu::ArchEndian,
        exception_behavior: ExceptionBehavior,
        loader: Arc<dyn Loader>,
        target_program: Cow<[u8]>,
        runtime: Handle,
        backend: Option<Backend>,
    ) -> Result<Arc<Self>, ProcessorBuilderImplError> {
        ...
    }
}

impl ProcessorImpl for ExampleCpu {
    fn cpu(&self) -> CpuBackend {
        self.cpu.clone()
    }

    fn cpu_stop(&self) -> Result<(), StyxMachineError> {
        ...
    }

    fn event_controller(&self) -> Arc<dyn EventController> {
        self.event_controller.clone()
    }

    fn cpu_start(
        &self,
        timeout: Option<Duration>,
        insns: Option<u64>,
    ) -> Result<TargetExitReason, StyxMachineError> {
        ...
    }

    fn initialize(&self) -> Result<(), StyxMachineError> {
        ...
    }

    fn populate_default_registers(
        &self,
        desc: &mut MemoryLoaderDesc,
    ) -> Result<(), StyxMachineError> {
        ...
    }

    fn setup_address_space(&self) -> Result<(), StyxMachineError> {
        ...
    }
}
```

#### After

```rust
pub struct ExampleCpuBuilder {}

impl ProcessorImpl for ExampleCpuBuilder {
    fn build(
        &self,
        _runtime: &ProcessorRuntime,
        cpu_backend: Backend,
    ) -> Result<ProcessorBundle, UnknownError> {
        ...
    }

    fn init(&self, proc: &mut BuildingProcessor) -> Result<(), UnknownError> {
        ...
    }
}
```

See `styx/processors/arm/styx-kinetis21-processor/src/lib.rs` for an example of what implementing this trait looks like in practice.

### Instantiating a Processor

The previous `ProcessorBuilder` had options for things like endianness, architecture, architecture variants, and the build method was generic with the processor being built.  In the new architecture, most of these options are intrinsic to the processor being built and as such they are handled by the `ProcessorImpl` passed to `ProcessorBuilder::with_builder()`.

#### Before

```rust
    let proc = ProcessorBuilder::default()
        .with_endian(ArchEndian::LittleEndian)
        .with_executor(Executor::default())
        .with_loader(RawLoader)
        .with_target_program(get_firmware_path())
        .with_variant(ArmVariants::ArmCortexM3)
        .build::<ExampleCpu>()?;
```

#### After

```rust
    let mut proc = ProcessorBuilder::default()
        .with_builder(ExampleCpuBuilder {})
        .with_target_program(get_firmware_path())
        .build()?;
```

To run the processor use `Processor::run()` with an `ExecutionConstraint`. The simplest one is `Forever`.

```rust
let mut proc = ProcessorBuilder::default()
        .with_builder(ExampleCpuBuilder {})
        .with_target_program(get_firmware_path())
        .build()?;

proc.run(Forever);
```

### Hooks

The ways of adding and removing hooks haven't really changed but the hook callback function prototypes have changed.  Instead of a `CpuBackend` as the first argument to hook callbacks, you now get a `CoreHandle` which bundles together the cpu, mmu, and event controller components as mutable references.

#### Before

```rust
fn code_hook_callback(cpu: CpuBackend) {
    // do something
}
```

#### After

```rust
fn code_hook_callback(proc: CoreHandle) -> Result<(), UnknownError> {
    // do something
    Ok(())
}
```

### Memory Access

The 1.0 introduces the Mmu to the processor. This defined an api for device specific address translation. There is also support for separate code/data memory as is needed by some architectures. For the Styx user this means there is no longer `read_memory()`/`write_memory()` and instead `read_code()`/`write_code()` and `read_data()`/`write_data()` for code and data memory regions respectively. On architectures with no distinction between code/data memory then they will operate the same.

Data can be read without checking mmu permissions with the `sudo_` variants: e.g. `sudo_read_code()`.

There is also an experimental, alternative memory api accessed by the `Mmu::code()` and `Mmu::data()` methods. An example is shown below.

#### Before

```rust
fn code_hook_callback(cpu: CpuBackend) {
    let mut buf = [0u8; 4];
    cpu.read_memory(0x1000, &mut buf).unwrap();
    let my_u32 = u32::from_le_bytes(&buf);
}
```

#### After

```rust
fn code_hook_callback(proc: CoreHandle) -> Result<(), UnknownError> {
    let mut buf = vec![0u8; 8];
    cpu.read_data(0x1000, &mut buf)?; // read from data region
    let my_u32 = u32::from_le_bytes(&buf);

    // or with experiment memory api
    // note we can also use ? operator to propagate errors

    let my_u32 = cpu.data().read(0x1000).le().u32()?;

    Ok(())
}
```

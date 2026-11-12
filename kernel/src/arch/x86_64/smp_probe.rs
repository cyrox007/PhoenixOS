//! Запуск вспомогательных процессоров самим ядром, только в QEMU.
//!
//! Эта проверка намеренно НЕ используется диагностическим образом ASUS.
//! Код не обращается к дискам и не запускает DMA. Процессоры после
//! чтения регистров остаются остановленными с выключенными прерываниями.
//! Снимок текущей CR3 не заменяет аудит остальных адресных пространств.

use core::arch::x86_64::{__cpuid, __cpuid_count};
use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

use phoenix_acpi::madt_topology::{MadtTopology, verify_observed_processors};
use phoenix_memory::SystemFrameAllocator;
use x86_64::{VirtAddr, registers::control::Cr3};

const MAX_CPUS: usize = 64;
const AP_STACK_SIZE: usize = 16 * 1024;
const MAX_VARIABLE_MTRRS: usize = 16;
const FIXED_MTRR: [u32; 11] = [
    0x250, 0x258, 0x259, 0x268, 0x269, 0x26a, 0x26b, 0x26c, 0x26d, 0x26e, 0x26f,
];
const IA32_APIC_BASE: u32 = 0x1b;
const X2APIC_ICR: u32 = 0x830;

const GDT_PTR: usize = 0xf00;
const FAR32: usize = 0xf08;
const FAR64: usize = 0xf10;
const TEMP_ROOT: usize = 0xf18;
const AP_STACK: usize = 0xf20;
const AP_ENTRY: usize = 0xf28;
const AP_SLOT: usize = 0xf30;
const KERNEL_ROOT: usize = 0xf38;
const GDT: usize = 0xf40;

global_asm!(
    ".section .text.phoenix_ap_trampoline, \"ax\"",
    ".global phoenix_ap_trampoline_begin",
    ".global phoenix_ap_trampoline_end",
    ".global phoenix_ap_trampoline_pm32",
    ".global phoenix_ap_trampoline_lm64",
    ".code16",
    "phoenix_ap_trampoline_begin:",
    "cli",
    "cld",
    "movw %cs, %ax",
    "movw %ax, %ds",
    "movzwl %ax, %esi",
    "shll $4, %esi",
    "lgdtl 0xF00",
    "movl %cr0, %eax",
    "orl $1, %eax",
    "movl %eax, %cr0",
    "ljmpl *0xF08",
    ".code32",
    "phoenix_ap_trampoline_pm32:",
    "movw $0x10, %ax",
    "movw %ax, %ds",
    "movw %ax, %es",
    "movw %ax, %ss",
    "movl $0x20, %eax",
    "movl %eax, %cr4",
    "movl 0xF18(%esi), %eax",
    "movl %eax, %cr3",
    "movl $0xC0000080, %ecx",
    "rdmsr",
    "orl $0x900, %eax",
    "wrmsr",
    "movl %cr0, %eax",
    "orl $0x80010001, %eax",
    "movl %eax, %cr0",
    "ljmpl *0xF10(%esi)",
    ".code64",
    "phoenix_ap_trampoline_lm64:",
    "movl %esi, %esi",
    "movq 0xF20(%rsi), %rsp",
    "movq 0xF30(%rsi), %rdi",
    "movq 0xF38(%rsi), %rsi",
    "movq 0xF28(%rsi), %rax",
    "xorq %rbp, %rbp",
    "jmpq *%rax",
    "phoenix_ap_trampoline_end:",
    ".text",
    options(att_syntax),
);

unsafe extern "C" {
    static phoenix_ap_trampoline_begin: u8;
    static phoenix_ap_trampoline_end: u8;
    static phoenix_ap_trampoline_pm32: u8;
    static phoenix_ap_trampoline_lm64: u8;
}

#[repr(align(16))]
struct Stacks([[u8; AP_STACK_SIZE]; MAX_CPUS]);

static mut STACKS: Stacks = Stacks([[0; AP_STACK_SIZE]; MAX_CPUS]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CpuRegisters {
    apic_id: u32,
    cr3: u64,
    pat: u64,
    mtrr_cap: u64,
    mtrr_default: u64,
    variable: [(u64, u64); MAX_VARIABLE_MTRRS],
    fixed: [u64; FIXED_MTRR.len()],
    amd_syscfg: u64,
    amd_topmem2: u64,
    amd_observed: bool,
    valid: bool,
}

impl CpuRegisters {
    const EMPTY: Self = Self {
        apic_id: 0,
        cr3: 0,
        pat: 0,
        mtrr_cap: 0,
        mtrr_default: 0,
        variable: [(0, 0); MAX_VARIABLE_MTRRS],
        fixed: [0; FIXED_MTRR.len()],
        amd_syscfg: 0,
        amd_topmem2: 0,
        amd_observed: false,
        valid: false,
    };

    fn cache_equal(&self, other: &Self) -> bool {
        self.pat == other.pat
            && self.mtrr_cap == other.mtrr_cap
            && self.mtrr_default == other.mtrr_default
            && self.variable == other.variable
            && self.fixed == other.fixed
            && self.amd_syscfg == other.amd_syscfg
            && self.amd_topmem2 == other.amd_topmem2
            && self.amd_observed == other.amd_observed
    }
}

struct Slot {
    ready: AtomicBool,
    registers: UnsafeCell<CpuRegisters>,
}

// Только одно конкретное ядро записывает слот. BSP читает после Release.
unsafe impl Sync for Slot {}

impl Slot {
    const fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            registers: UnsafeCell::new(CpuRegisters::EMPTY),
        }
    }

    fn publish(&self, registers: CpuRegisters) {
        unsafe { *self.registers.get() = registers };
        self.ready.store(true, Ordering::Release);
    }

    fn read(&self) -> Option<CpuRegisters> {
        if !self.ready.load(Ordering::Acquire) {
            return None;
        }
        Some(unsafe { *self.registers.get() })
    }
}

static OBSERVED: [Slot; MAX_CPUS] = [const { Slot::new() }; MAX_CPUS];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeError {
    NotQemu,
    InvalidTopology,
    X2ApicUnavailable,
    LowPageUnavailable,
    PageTablesUnavailable,
    TrampolineTooLarge,
    BadPhysicalAddress,
    ApDidNotStart { apic_id: u32 },
    IncompleteMeasurements,
    ConflictingRegisters,
    WrongCr3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeResult {
    pub expected: usize,
    pub measured: usize,
    pub distinct: bool,
    pub cache_agrees: bool,
    pub cr3_agrees: bool,
}

/// Код запуска никогда не вызывается на физической машине.
/// Гипервизор проверяется дополнительно к изолированному сценарию CI.
fn is_qemu() -> bool {
    if __cpuid(1).ecx & (1 << 31) == 0 {
        return false;
    }
    let hypervisor = __cpuid(0x4000_0000);
    let mut vendor = [0u8; 12];
    vendor[..4].copy_from_slice(&hypervisor.ebx.to_le_bytes());
    vendor[4..8].copy_from_slice(&hypervisor.ecx.to_le_bytes());
    vendor[8..12].copy_from_slice(&hypervisor.edx.to_le_bytes());
    vendor.starts_with(b"KVMKVMKVM") || vendor == *b"TCGTCGTCGTCG"
}

fn running_apic() -> Option<u32> {
    let max = __cpuid(0).eax;
    for leaf in [0x1f, 0x0b] {
        if max < leaf {
            continue;
        }
        let info = __cpuid_count(leaf, 0);
        if info.ebx & 0xffff != 0 && (info.ecx >> 8) & 0xff != 0 {
            return Some(info.edx);
        }
    }
    let cpu = __cpuid(1);
    if cpu.edx & (1 << 9) == 0 {
        return None;
    }
    Some(cpu.ebx >> 24)
}

unsafe fn read_msr(index: u32) -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        asm!(
            "rdmsr",
            in("ecx") index,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }
    (u64::from(high) << 32) | u64::from(low)
}

unsafe fn write_msr(index: u32, value: u64) {
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") index,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nomem, nostack, preserves_flags),
        );
    }
}

fn snapshot() -> CpuRegisters {
    let mut sample = CpuRegisters::EMPTY;
    let Some(id) = running_apic() else {
        return sample;
    };
    let cpu = __cpuid(1);
    if cpu.edx & ((1 << 12) | (1 << 16)) != (1 << 12) | (1 << 16) {
        return sample;
    }

    sample.apic_id = id;
    sample.cr3 = Cr3::read().0.start_address().as_u64();
    sample.pat = unsafe { read_msr(0x277) };
    sample.mtrr_cap = unsafe { read_msr(0xfe) };
    sample.mtrr_default = unsafe { read_msr(0x2ff) };
    let count = (sample.mtrr_cap & 0xff) as usize;
    if count > MAX_VARIABLE_MTRRS || sample.cr3 == 0 || sample.cr3 & 4095 != 0 {
        return sample;
    }
    for (index, pair) in sample.variable[..count].iter_mut().enumerate() {
        let register = 0x200 + (index as u32) * 2;
        *pair = (unsafe { read_msr(register) }, unsafe {
            read_msr(register + 1)
        });
    }
    if sample.mtrr_cap & (1 << 8) != 0 && sample.mtrr_default & (1 << 10) != 0 {
        for (slot, register) in sample.fixed.iter_mut().zip(FIXED_MTRR) {
            *slot = unsafe { read_msr(register) };
        }
    }

    // AMD SYSCFG/TOPMEM2 намеренно не читаются в виртуализации:
    // QEMU может не поддерживать это семейство MSR.
    sample.valid = true;
    sample
}

extern "C" fn ap_entry(slot: usize, kernel_root: u64) -> ! {
    unsafe {
        asm!("mov cr3, {}", in(reg) kernel_root, options(nostack, preserves_flags));
    }
    let sample = snapshot();
    if slot < MAX_CPUS {
        OBSERVED[slot].publish(sample);
    }

    // Обработчик никогда не возвращается на стек без точки возврата.
    // При выключенных прерываниях AP больше не обращается к памяти.
    loop {
        x86_64::instructions::hlt();
    }
}

fn read_tsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

fn delay_cycles(cycles: u64) {
    let start = read_tsc();
    while read_tsc().wrapping_sub(start) < cycles {
        core::hint::spin_loop();
    }
}

fn wait_ready(index: usize, cycles: u64) -> bool {
    let start = read_tsc();
    loop {
        if OBSERVED[index].read().is_some() {
            return true;
        }
        if read_tsc().wrapping_sub(start) >= cycles {
            return false;
        }
        core::hint::spin_loop();
    }
}

/// Проверка изоляции и полного охвата выполняется независимо от
/// гипервизора; функция не управляет оборудованием.
fn verify(topology: &MadtTopology, kernel_root: u64) -> Result<ProbeResult, ProbeError> {
    let expected = topology.enabled_apic_ids();
    let mut ids = [0u32; MAX_CPUS];
    let mut snapshots = [CpuRegisters::EMPTY; MAX_CPUS];

    for index in 0..expected.len() {
        let Some(sample) = OBSERVED[index].read() else {
            return Err(ProbeError::IncompleteMeasurements);
        };
        if !sample.valid {
            return Err(ProbeError::IncompleteMeasurements);
        }
        ids[index] = sample.apic_id;
        snapshots[index] = sample;
    }
    let coverage = verify_observed_processors(topology, &ids[..expected.len()])
        .map_err(|_| ProbeError::InvalidTopology)?;
    if !coverage.identifiers_complete {
        return Err(ProbeError::IncompleteMeasurements);
    }
    let first = snapshots[0];
    let cache_agrees = snapshots[..expected.len()]
        .iter()
        .all(|sample| first.cache_equal(sample));
    let cr3_agrees = snapshots[..expected.len()]
        .iter()
        .all(|sample| sample.cr3 == kernel_root);
    if !cache_agrees {
        return Err(ProbeError::ConflictingRegisters);
    }
    if !cr3_agrees {
        return Err(ProbeError::WrongCr3);
    }
    Ok(ProbeResult {
        expected: expected.len(),
        measured: coverage.observed,
        distinct: coverage.identifiers_complete,
        cache_agrees,
        cr3_agrees,
    })
}

/// Создаёт временное отображение младших 2 МиБ и сохраняет верхнюю
/// адресацию ядра. Временные страницы не отдаются распределителю:
/// поздно запущенный AP не сможет попасть в освобождённую память.
fn prepare_tables<const N: usize>(
    frames: &mut SystemFrameAllocator<N>,
    offset: VirtAddr,
    kernel_root: u64,
) -> Result<u64, ProbeError> {
    let root = frames
        .allocate_contiguous_below_4k(3, 1, 0xffff_ffff)
        .ok_or(ProbeError::PageTablesUnavailable)?
        .start_address()
        .as_u64();
    if root == 0 || root & 4095 != 0 {
        return Err(ProbeError::BadPhysicalAddress);
    }
    let pointer = |physical: u64| -> *mut u64 { (offset + physical).as_mut_ptr::<u64>() };
    unsafe {
        ptr::copy_nonoverlapping(pointer(kernel_root), pointer(root), 512);
        ptr::write_bytes(pointer(root + 4096), 0, 512);
        ptr::write_bytes(pointer(root + 8192), 0, 512);
        pointer(root).write_volatile(root + 4096 | 3);
        pointer(root + 4096).write_volatile(root + 8192 | 3);
        pointer(root + 8192).write_volatile(0x83);
    }
    Ok(root)
}

fn write_trampoline(
    base: u64,
    offset: VirtAddr,
    temporary_root: u64,
    kernel_root: u64,
) -> Result<(), ProbeError> {
    let start = ptr::addr_of!(phoenix_ap_trampoline_begin) as usize;
    let end = ptr::addr_of!(phoenix_ap_trampoline_end) as usize;
    let length = end
        .checked_sub(start)
        .ok_or(ProbeError::TrampolineTooLarge)?;
    if length == 0 || length >= GDT_PTR {
        return Err(ProbeError::TrampolineTooLarge);
    }

    let relative = |symbol: *const u8| -> Result<u32, ProbeError> {
        let location = (symbol as usize)
            .checked_sub(start)
            .ok_or(ProbeError::TrampolineTooLarge)?;
        let location = u32::try_from(location).map_err(|_| ProbeError::TrampolineTooLarge)?;
        u32::try_from(base)
            .ok()
            .and_then(|addr| addr.checked_add(location))
            .ok_or(ProbeError::BadPhysicalAddress)
    };
    let pm32 = relative(ptr::addr_of!(phoenix_ap_trampoline_pm32))?;
    let lm64 = relative(ptr::addr_of!(phoenix_ap_trampoline_lm64))?;
    let dest = (offset + base).as_mut_ptr::<u8>();
    unsafe {
        ptr::write_bytes(dest, 0, 4096);
        ptr::copy_nonoverlapping(start as *const u8, dest, length);
        ptr::write_unaligned(dest.add(GDT_PTR).cast::<u16>(), 31);
        ptr::write_unaligned(
            dest.add(GDT_PTR + 2).cast::<u32>(),
            base as u32 + GDT as u32,
        );
        ptr::write_unaligned(dest.add(FAR32).cast::<u32>(), pm32);
        ptr::write_unaligned(dest.add(FAR32 + 4).cast::<u16>(), 8);
        ptr::write_unaligned(dest.add(FAR64).cast::<u32>(), lm64);
        ptr::write_unaligned(dest.add(FAR64 + 4).cast::<u16>(), 24);
        ptr::write_unaligned(dest.add(TEMP_ROOT).cast::<u32>(), temporary_root as u32);
        ptr::write_unaligned(dest.add(AP_ENTRY).cast::<u64>(), ap_entry as usize as u64);
        ptr::write_unaligned(dest.add(KERNEL_ROOT).cast::<u64>(), kernel_root);
        for (index, descriptor) in [
            0,
            0x00cf_9a00_0000_ffff,
            0x00cf_9200_0000_ffff,
            0x00af_9a00_0000_ffff,
        ]
        .into_iter()
        .enumerate()
        {
            ptr::write_unaligned(dest.add(GDT + index * 8).cast::<u64>(), descriptor);
        }
    }
    Ok(())
}

fn send_ipi(apic_id: u32, command: u32) {
    unsafe { write_msr(X2APIC_ICR, ((apic_id as u64) << 32) | command as u64) };
}

/// Запускает AP строго по одному в проверочном QEMU. Любое отсутствие
/// измерения означает ошибку, а не частичный успех.
pub fn run<const N: usize>(
    topology: &MadtTopology,
    frames: &mut SystemFrameAllocator<N>,
    offset: VirtAddr,
) -> Result<ProbeResult, ProbeError> {
    if !is_qemu() {
        return Err(ProbeError::NotQemu);
    }
    let ids = topology.enabled_apic_ids();
    if ids.is_empty() || ids.len() > MAX_CPUS || ids.len() < 2 {
        return Err(ProbeError::InvalidTopology);
    }
    if __cpuid(1).ecx & (1 << 21) == 0 {
        return Err(ProbeError::X2ApicUnavailable);
    }

    // Не включаем таймер или прерывания; допускается только x2APIC ICR.
    let old_base = unsafe { read_msr(IA32_APIC_BASE) };
    if old_base & (1 << 11) == 0 {
        return Err(ProbeError::X2ApicUnavailable);
    }
    if old_base & (1 << 10) == 0 {
        unsafe { write_msr(IA32_APIC_BASE, old_base | 1 << 10) };
    }
    let kernel_root = Cr3::read().0.start_address().as_u64();
    let Some(bsp_id) = running_apic() else {
        return Err(ProbeError::InvalidTopology);
    };
    let Some(bsp_slot) = ids.iter().position(|id| *id == bsp_id) else {
        return Err(ProbeError::InvalidTopology);
    };

    // Начальная страница 0 физической памяти никогда не используется
    // как SIPI-вектор. Распределитель может считать её свободной,
    // поэтому резервируем и пропускаем её без освобождения.
    let mut trampoline = frames
        .allocate_contiguous_below_4k(1, 1, 0x0f_ffff)
        .ok_or(ProbeError::LowPageUnavailable)?
        .start_address()
        .as_u64();
    if trampoline == 0 {
        trampoline = frames
            .allocate_contiguous_below_4k(1, 1, 0x0f_ffff)
            .ok_or(ProbeError::LowPageUnavailable)?
            .start_address()
            .as_u64();
    }
    if !(0x1000..=0xff000).contains(&trampoline) {
        return Err(ProbeError::BadPhysicalAddress);
    }
    let temporary_root = prepare_tables(frames, offset, kernel_root)?;
    write_trampoline(trampoline, offset, temporary_root, kernel_root)?;
    crate::serial::line(
        &mut crate::serial::console(),
        "INFO",
        format_args!(
            "kernel AP stage: trampoline={trampoline:#x} temporary_cr3={temporary_root:#x} kernel_cr3={kernel_root:#x} bsp={bsp_id}"
        ),
    );

    OBSERVED[bsp_slot].publish(snapshot());
    let data = (offset + trampoline).as_mut_ptr::<u8>();
    for (slot, id) in ids.iter().enumerate() {
        if *id == bsp_id {
            continue;
        }
        let stack_top = unsafe {
            let array = ptr::addr_of!(STACKS).cast::<u8>();
            array.add((slot + 1) * AP_STACK_SIZE) as u64
        };
        unsafe {
            ptr::write_unaligned(data.add(AP_STACK).cast::<u64>(), stack_top);
            ptr::write_unaligned(data.add(AP_SLOT).cast::<u64>(), slot as u64);
        }

        crate::serial::line(
            &mut crate::serial::console(),
            "INFO",
            format_args!("kernel AP stage: launching apic={id} slot={slot}"),
        );
        send_ipi(*id, 0xc500);
        delay_cycles(30_000_000);
        send_ipi(*id, 0x8500);
        delay_cycles(300_000);
        send_ipi(*id, 0x4600 | (trampoline as u32 >> 12));
        if !wait_ready(slot, 300_000_000) {
            send_ipi(*id, 0x4600 | (trampoline as u32 >> 12));
            if !wait_ready(slot, 900_000_000) {
                return Err(ProbeError::ApDidNotStart { apic_id: *id });
            }
        }
        crate::serial::line(
            &mut crate::serial::console(),
            "INFO",
            format_args!("kernel AP stage: observed apic={id} slot={slot}"),
        );
    }
    verify(topology, kernel_root)
}

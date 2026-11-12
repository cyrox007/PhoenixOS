#![no_std]
#![no_main]

// Отдельная предзагрузочная диагностика. Никаких обращений к накопителям,
// PCI, DMA или IOMMU; работа с дополнительными ядрами только через UEFI MP.
use core::arch::x86_64::{__cpuid, __cpuid_count};
use core::arch::asm;
use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use core::time::Duration;
use uefi::proto::pi::mp::MpServices;
use uefi::{Status, boot};

const MAX_PROCESSORS: usize = 64;
const MAX_VARIABLE_MTRRS: usize = 16;
const FIXED_MTRR_REGS: [u32; 11] = [
    0x250, 0x258, 0x259, 0x268, 0x269, 0x26a, 0x26b, 0x26c, 0x26d, 0x26e, 0x26f,
];

#[derive(Clone, Copy)]
struct CpuSnapshot {
    apic_id: u32,
    cr3: u64,
    pat: u64,
    mtrr_cap: u64,
    mtrr_default: u64,
    variable: [(u64, u64); MAX_VARIABLE_MTRRS],
    variable_count: usize,
    fixed: [u64; FIXED_MTRR_REGS.len()],
    amd_syscfg: u64,
    amd_top_mem2: u64,
    amd_observed: bool,
    valid: bool,
}

impl CpuSnapshot {
    const EMPTY: Self = Self {
        apic_id: 0,
        cr3: 0,
        pat: 0,
        mtrr_cap: 0,
        mtrr_default: 0,
        variable: [(0, 0); MAX_VARIABLE_MTRRS],
        variable_count: 0,
        fixed: [0; FIXED_MTRR_REGS.len()],
        amd_syscfg: 0,
        amd_top_mem2: 0,
        amd_observed: false,
        valid: false,
    };

    fn compatible_cache(&self, other: &Self) -> bool {
        self.pat == other.pat
            && self.mtrr_cap == other.mtrr_cap
            && self.mtrr_default == other.mtrr_default
            && self.variable_count == other.variable_count
            && self.variable == other.variable
            && self.fixed == other.fixed
            && self.amd_observed == other.amd_observed
            && self.amd_syscfg == other.amd_syscfg
            && self.amd_top_mem2 == other.amd_top_mem2
    }
}

struct Slot {
    ready: AtomicBool,
    snapshot: UnsafeCell<CpuSnapshot>,
}

// Только один процессор получает каждый индекс через fetch_add.
// После завершения процедуры BSP читает запись с барьером Acquire.
unsafe impl Sync for Slot {}

impl Slot {
    const fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            snapshot: UnsafeCell::new(CpuSnapshot::EMPTY),
        }
    }

    fn store(&self, snapshot: CpuSnapshot) {
        // SAFETY: индекс выдаётся единственному исполнителю; BSP ждёт ready.
        unsafe { *self.snapshot.get() = snapshot };
        self.ready.store(true, Ordering::Release);
    }

    fn load(&self) -> Option<CpuSnapshot> {
        self.ready
            .load(Ordering::Acquire)
            .then(|| unsafe { *self.snapshot.get() })
    }
}

static SAMPLE_COUNT: AtomicUsize = AtomicUsize::new(0);
static SAMPLES: [Slot; MAX_PROCESSORS] = [const { Slot::new() }; MAX_PROCESSORS];

struct Serial;

impl Write for Serial {
    fn write_str(&mut self, message: &str) -> fmt::Result {
        for byte in message.bytes() {
            if byte == b'\n' {
                uart_put(b'\r');
            }
            uart_put(byte);
        }
        Ok(())
    }
}

macro_rules! report {
    ($($arg:tt)*) => {
        let _ = writeln!(Serial, $($arg)*);
    };
}

fn uart_write(port: u16, value: u8) {
    // SAFETY: выделенный последовательный порт виртуальной машины QEMU.
    unsafe { asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags)) }
}

fn uart_read(port: u16) -> u8 {
    let value: u8;
    // SAFETY: только чтение регистра состояния виртуального UART.
    unsafe { asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags)) };
    value
}

fn uart_put(byte: u8) {
    // Ограниченное ожидание предотвращает зависание при неисправном UART.
    for _ in 0..100_000 {
        if uart_read(0x3fd) & 0x20 != 0 {
            uart_write(0x3f8, byte);
            return;
        }
        core::hint::spin_loop();
    }
}

fn uart_init() {
    uart_write(0x3f9, 0);
    uart_write(0x3fb, 0x80);
    uart_write(0x3f8, 1);
    uart_write(0x3f9, 0);
    uart_write(0x3fb, 3);
    uart_write(0x3fa, 0xc7);
    uart_write(0x3fc, 3);
}

/// Независимый признак виртуализации: никаких команд пуска AP на физическом
/// оборудовании даже при ошибочном выборе этого UEFI-файла.
fn known_qemu() -> bool {
    if __cpuid(1).ecx & (1 << 31) == 0 {
        return false;
    }
    let vendor = __cpuid(0x4000_0000);
    let mut id = [0u8; 12];
    id[..4].copy_from_slice(&vendor.ebx.to_le_bytes());
    id[4..8].copy_from_slice(&vendor.ecx.to_le_bytes());
    id[8..].copy_from_slice(&vendor.edx.to_le_bytes());
    id.starts_with(b"KVMKVMKVM") || id == *b"TCGTCGTCGTCG"
}

fn running_apic_id() -> Option<u32> {
    let max = __cpuid(0).eax;
    for leaf in [0x1f, 0x0b] {
        if max < leaf {
            continue;
        }
        let topology = __cpuid_count(leaf, 0);
        if topology.ebx & 0xffff != 0 && (topology.ecx >> 8) & 0xff != 0 {
            return Some(topology.edx);
        }
    }
    let vendor = __cpuid(0);
    if vendor.ebx == 0x6874_7541
        && vendor.edx == 0x6974_6e65
        && vendor.ecx == 0x444d_4163
        && __cpuid(0x8000_0000).eax >= 0x8000_001e
    {
        return Some(__cpuid(0x8000_001e).eax);
    }
    if __cpuid(1).edx & (1 << 9) != 0 {
        return Some(__cpuid(1).ebx >> 24);
    }
    None
}

unsafe fn read_msr(index: u32) -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: допустимость семейства MSR проверена через CPUID/MTRRCAP.
    unsafe {
        asm!(
            "rdmsr", in("ecx") index,
            out("eax") low, out("edx") high,
            options(nomem, nostack, preserves_flags)
        );
    }
    (u64::from(high) << 32) | u64::from(low)
}

fn read_current_cpu() -> CpuSnapshot {
    let mut data = CpuSnapshot::EMPTY;
    let Some(id) = running_apic_id() else {
        return data;
    };
    data.apic_id = id;
    let leaf = __cpuid(1);
    if leaf.edx & ((1 << 12) | (1 << 16)) != (1 << 12) | (1 << 16) {
        return data;
    }

    // CR3 относится к фазе UEFI, а не к последующему ядру PhoenixOS.
    unsafe { asm!("mov {}, cr3", out(reg) data.cr3, options(nomem, nostack, preserves_flags)) };
    data.pat = unsafe { read_msr(0x277) };
    data.mtrr_cap = unsafe { read_msr(0xfe) };
    data.mtrr_default = unsafe { read_msr(0x2ff) };
    data.variable_count = (data.mtrr_cap & 0xff) as usize;
    if data.cr3 == 0 || data.cr3 & 4095 != 0 || data.variable_count > MAX_VARIABLE_MTRRS {
        return data;
    }

    for (index, pair) in data.variable[..data.variable_count].iter_mut().enumerate() {
        let register = 0x200 + 2 * index as u32;
        *pair = (unsafe { read_msr(register) }, unsafe {
            read_msr(register + 1)
        });
    }
    if data.mtrr_cap & (1 << 8) != 0 && data.mtrr_default & (1 << 10) != 0 {
        for (slot, register) in data.fixed.iter_mut().zip(FIXED_MTRR_REGS) {
            *slot = unsafe { read_msr(register) };
        }
    }

    // Только QEMU: специфичные AMD-регистры не читаем, если гостевая
    // виртуализация может вызвать #GP или синтезировать неполные значения.
    // Для реальной приёмки AMD нужен отдельный проверенный режим.
    data.valid = true;
    data
}

fn measure_cpu() {
    let index = SAMPLE_COUNT.fetch_add(1, Ordering::AcqRel);
    if index < MAX_PROCESSORS {
        SAMPLES[index].store(read_current_cpu());
    }
}

extern "efiapi" fn measure_ap(_argument: *mut c_void) {
    // На вспомогательном ЦП не используем услуги UEFI и не пишем в UART.
    measure_cpu();
}

fn execute() -> Status {
    uart_init();
    report!("AP_PROBE mode=UEFI-QEMU readonly=true hardware_dma=false");
    let handle = match boot::get_handle_for_protocol::<MpServices>() {
        Ok(handle) => handle,
        Err(_) => {
            report!("AP_PROBE error=mp-services-unavailable");
            return Status::UNSUPPORTED;
        }
    };
    let mp = match boot::open_protocol_exclusive::<MpServices>(handle) {
        Ok(mp) => mp,
        Err(_) => {
            report!("AP_PROBE error=mp-services-open");
            return Status::UNSUPPORTED;
        }
    };
    let count = match mp.get_number_of_processors() {
        Ok(count) => count,
        Err(_) => {
            report!("AP_PROBE error=processor-count");
            return Status::ABORTED;
        }
    };
    if count.enabled == 0 || count.enabled > MAX_PROCESSORS || count.total > MAX_PROCESSORS {
        report!("AP_PROBE error=capacity total={} enabled={}", count.total, count.enabled);
        return Status::ABORTED;
    }

    let mut enabled_ids = [None; MAX_PROCESSORS];
    let mut bsp_count = 0;
    for index in 0..count.total {
        let info = match mp.get_processor_info(index) {
            Ok(info) => info,
            Err(_) => return Status::ABORTED,
        };
        if !info.is_enabled() {
            continue;
        }
        let Ok(id) = u32::try_from(info.processor_id) else {
            report!("AP_PROBE error=apic-id-width");
            return Status::ABORTED;
        };
        if enabled_ids[..index].contains(&Some(id)) {
            report!("AP_PROBE error=duplicate-firmware-id");
            return Status::ABORTED;
        }
        enabled_ids[index] = Some(id);
        bsp_count += usize::from(info.is_bsp());
    }
    if bsp_count != 1 || mp.who_am_i().is_err() {
        report!("AP_PROBE error=invalid-bsp");
        return Status::ABORTED;
    }

    measure_cpu();
    if count.enabled > 1
        && mp
            .startup_all_aps(
                false,
                measure_ap,
                core::ptr::null_mut(),
                None,
                Some(Duration::from_secs(12)),
            )
            .is_err()
    {
        report!("AP_PROBE error=ap-timeout-or-failure");
        return Status::ABORTED;
    }

    let captured = SAMPLE_COUNT.load(Ordering::Acquire);
    let mut observed = [None; MAX_PROCESSORS];
    let mut valid = captured == count.enabled;
    let mut unique = true;
    let mut cache_consensus = true;
    let first = SAMPLES[0].load();
    for index in 0..captured.min(MAX_PROCESSORS) {
        let Some(sample) = SAMPLES[index].load() else {
            valid = false;
            continue;
        };
        observed[index] = Some(sample.apic_id);
        let found = enabled_ids[..count.total].contains(&Some(sample.apic_id));
        let repeated = observed[..index].contains(&Some(sample.apic_id));
        unique &= found && !repeated;
        valid &= sample.valid;
        cache_consensus &= first
            .as_ref()
            .is_some_and(|previous| previous.compatible_cache(&sample));
        report!(
            "AP_PROBE cpu={} apic={} cr3={:#x} pat={:#x} mtrr_default={:#x} mtrr_count={} amd_observed={} valid={}",
            index,
            sample.apic_id,
            sample.cr3,
            sample.pat,
            sample.mtrr_default,
            sample.variable_count,
            sample.amd_observed,
            sample.valid
        );
    }

    let complete = valid && unique && cache_consensus;
    report!(
        "AP_PROBE result={} total={} enabled={} captured={} unique={} cache_consensus={} phase=uefi-before-exit kernel_cr3_verified=false dma_authorized=false",
        if complete { "OK" } else { "INCOMPLETE" },
        count.total,
        count.enabled,
        captured,
        unique,
        cache_consensus
    );
    if complete { Status::SUCCESS } else { Status::ABORTED }
}

#[uefi::entry]
fn main() -> Status {
    // Даже обращение к UART не выполняем на неподтверждённой системе.
    if !known_qemu() {
        return Status::UNSUPPORTED;
    }
    execute()
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

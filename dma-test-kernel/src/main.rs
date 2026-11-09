#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

//! Отдельное ядро для фактической DMA-передачи только в QEMU.
//! Не содержит драйверов накопителей и никогда не запускается на ASUS.
//! Защита гипервизора выполняется до любых команд настройки PCI и IOMMU.

use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::{BootInfo, entry_point};
use core::arch::x86_64::__cpuid;
use core::panic::PanicInfo;
use lazy_static::lazy_static;
use phoenix_memory::SystemFrameAllocator;
use phoenix_pcie::{ConfigAccess, EcamSegment, PcieFunction};
use phoenix_reference_edu::{EDU_DEVICE_ID, EDU_VENDOR_ID};
use phoenix_vm::ActivePageTable;
use x86_64::VirtAddr;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

#[path = "../../kernel/src/acpi.rs"]
mod acpi;
#[path = "../../kernel/src/reference_edu.rs"]
mod reference_edu;
#[path = "../../kernel/src/serial.rs"]
mod serial;

mod arch {
    pub mod x86_64 {
        pub mod apic {
            pub const REFERENCE_EDU_VECTOR: u8 = 0xe1;

            pub fn acknowledge_external_interrupt() {
                let low: u32 = 0;
                unsafe {
                    core::arch::asm!(
                        "wrmsr",
                        in("ecx") 0x80b_u32,
                        in("eax") low,
                        in("edx") 0_u32,
                        options(nostack, preserves_flags),
                    );
                }
            }
        }
    }
}

const MAX_MEMORY_REGIONS: usize = 256;
const IA32_APIC_BASE: u32 = 0x1b;
const IA32_X2APIC_SPURIOUS: u32 = 0x80f;

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt[usize::from(arch::x86_64::apic::REFERENCE_EDU_VECTOR)]
            .set_handler_fn(reference_edu::interrupt_handler);
        idt[0xff].set_handler_fn(spurious_interrupt);
        idt
    };
}

extern "x86-interrupt" fn spurious_interrupt(_frame: InterruptStackFrame) {}

pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.kernel_stack_size = 128 * 1024;
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

/// Сначала убеждаемся, что это KVM/TCG QEMU; нет подтверждения — нет записи
/// в PCI/регистры APIC и нет начала DMA.
fn is_test_hypervisor() -> bool {
    let leaf = __cpuid(1);
    if leaf.ecx & (1 << 31) == 0 {
        return false;
    }
    let vendor = __cpuid(0x4000_0000);
    let mut name = [0u8; 12];
    name[0..4].copy_from_slice(&vendor.ebx.to_le_bytes());
    name[4..8].copy_from_slice(&vendor.ecx.to_le_bytes());
    name[8..12].copy_from_slice(&vendor.edx.to_le_bytes());
    &name == b"KVMKVMKVM\0\0\0"
        || &name == b"TCGTCGTCG\0\0"
        || &name == b"QEMUQEMUQEMU"
}

unsafe fn read_msr(register: u32) -> u64 {
    let (low, high): (u32, u32);
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") register,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }
    (u64::from(high) << 32) | u64::from(low)
}

unsafe fn write_msr(register: u32, value: u64) {
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") register,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nostack, preserves_flags),
        );
    }
}

fn initialize_edu_interrupt() -> Result<(), &'static str> {
    let features = __cpuid(1);
    if features.edx & (1 << 9) == 0 || features.ecx & (1 << 21) == 0 {
        return Err("x2APIC unavailable");
    }
    x86_64::instructions::interrupts::disable();
    IDT.load();

    // Только после is_test_hypervisor() и обнаружения ровно одного EDU.
    let mut primary = x86_64::instructions::port::Port::<u8>::new(0x21);
    let mut secondary = x86_64::instructions::port::Port::<u8>::new(0xa1);
    unsafe {
        primary.write(0xff);
        secondary.write(0xff);
    }
    let old_base = unsafe { read_msr(IA32_APIC_BASE) };
    unsafe { write_msr(IA32_APIC_BASE, old_base | (1 << 11) | (1 << 10)) };
    let spurious = unsafe { read_msr(IA32_X2APIC_SPURIOUS) };
    unsafe { write_msr(IA32_X2APIC_SPURIOUS, spurious | (1 << 8) | 0xff) };
    x86_64::instructions::interrupts::enable();
    Ok(())
}

struct ReadOnlyEcam {
    offset: VirtAddr,
}

impl ConfigAccess for ReadOnlyEcam {
    fn read_u32(&self, physical_address: u64) -> u32 {
        let address = self.offset + physical_address;
        unsafe { address.as_ptr::<u32>().read_volatile() }
    }
}

fn find_unique_edu(
    mcfg: &phoenix_acpi::McfgInfo,
    offset: VirtAddr,
) -> Result<PcieFunction, &'static str> {
    let access = ReadOnlyEcam { offset };
    let mut found = None;
    let mut matches = 0u32;

    for entry in mcfg.iter() {
        let segment = EcamSegment::new(
            entry.base_address,
            entry.segment_group,
            entry.start_bus,
            entry.end_bus,
        )
        .map_err(|_| "invalid ECAM segment")?;
        phoenix_pcie::enumerate_segment(&access, segment, |function| {
            if function.vendor_id != EDU_VENDOR_ID || function.device_id != EDU_DEVICE_ID {
                return;
            }
            matches = matches.saturating_add(1);
            found = Some(function);
        })
        .map_err(|_| "ECAM enumeration failed")?;
    }

    if matches != 1 {
        return Err("expected exactly one virtual QEMU EDU");
    }
    found.ok_or("virtual QEMU EDU missing")
}

fn kernel_main(info: &'static mut BootInfo) -> ! {
    let mut out = serial::console();
    serial::line(
        &mut out,
        "INFO",
        format_args!("DMA separate QEMU profile: entered storage_drivers=none"),
    );

    if !is_test_hypervisor() {
        serial::line(
            &mut out,
            "WARN",
            format_args!("DMA separate QEMU profile: denied not-qemu"),
        );
        halt();
    }

    let offset = info
        .physical_memory_offset
        .into_option()
        .map(VirtAddr::new)
        .expect("missing physical memory mapping");
    let rsdp = info.rsdp_addr.into_option().expect("missing ACPI");
    let acpi = acpi::discover(rsdp, offset).expect("ACPI unavailable");
    let function = find_unique_edu(&acpi.mcfg, offset).expect("EDU not unique");
    serial::line(
        &mut out,
        "INFO",
        format_args!(
            "DMA separate QEMU profile: gated vendor={:04x} device={:04x} bus={} pci-storage=unused",
            function.vendor_id, function.device_id, function.bus
        ),
    );

    initialize_edu_interrupt().expect("QEMU x2APIC/MSI not initialized");
    let mut frames = SystemFrameAllocator::<MAX_MEMORY_REGIONS>::from_regions(
        &info.memory_regions,
    )
    .expect("no physical allocator");
    let mut page_table = unsafe { ActivePageTable::from_current(offset) };
    let result = unsafe { reference_edu::self_test(function, &mut page_table, &mut frames, offset) }
        .expect("real QEMU EDU DMA transfer failed");

    if result.verified_bytes != phoenix_reference_edu::EDU_TRANSFER_BYTES
        || result.returned_dma_pages != 1
        || result.irq_mask
            & (phoenix_reference_edu::EDU_IRQ_PROBE | phoenix_reference_edu::EDU_IRQ_DMA)
            != phoenix_reference_edu::EDU_IRQ_PROBE | phoenix_reference_edu::EDU_IRQ_DMA
    {
        panic!("DMA result failed verification");
    }
    serial::line(
        &mut out,
        "INFO",
        format_args!(
            "DMA QEMU EDU real transfer: OK bytes={} returned_pages={} irq={:#x}",
            result.verified_bytes, result.returned_dma_pages, result.irq_mask
        ),
    );
    serial::line(
        &mut out,
        "INFO",
        format_args!("DMA QEMU-only complete: OK physical-asus=denied iommu-domain=unverified"),
    );
    halt()
}

fn halt() -> ! {
    x86_64::instructions::interrupts::disable();
    loop {
        x86_64::instructions::hlt();
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    serial::emergency(format_args!("[PANIC] DMA QEMU-only: {info}\n"));
    halt()
}

//! Пассивное обследование ЦП, PAT/MTRR, ACPI и PCIe.
//! Здесь нет распределителя DMA, драйверов устройств или записи в PCI/MSR.

use crate::screen::Report;
use bootloader_api::BootInfo;
use bootloader_api::info::MemoryRegionKind;
use core::arch::x86_64::__cpuid;
use phoenix_acpi::{McfgInfo, RSDP_V2_SIZE, Rsdp, SDT_HEADER_SIZE, SdtHeader, Xsdt};
use phoenix_pcie::{ConfigAccess, EcamSegment};
use phoenix_vm::dma_cache::{
    DmaCacheSnapshot, FIXED_MTRR_REGISTERS, MAX_VARIABLE_MTRRS, memory_type_name,
};
use x86_64::VirtAddr;

const PHYSICAL_MAPPING_MINIMUM: u64 = 0x1_0000_0000;
const MAX_ACPI_TABLE: usize = 1024 * 1024;
const IA32_MTRRCAP: u32 = 0xfe;
const IA32_PAT: u32 = 0x277;
const IA32_MTRR_DEFAULT: u32 = 0x2ff;

#[derive(Clone, Copy, Debug)]
enum ReadError {
    MissingMapping,
    BadAddress,
    InvalidAcpi,
 }

pub struct HardwareContext {
    rsdp: Option<u64>,
    physical_offset: Option<u64>,
    physical_end: u64,
    ram_samples: [Option<u64>; 2],
}

impl HardwareContext {
    pub fn from_boot_info(info: &BootInfo) -> Self {
        Self {
            rsdp: info.rsdp_addr.into_option(),
            physical_offset: info.physical_memory_offset.into_option(),
            physical_end: info
                .memory_regions
                .iter()
                .fold(PHYSICAL_MAPPING_MINIMUM, |end, region| end.max(region.end)),
            ram_samples: select_ram_samples(info),
        }
    }
}

/// Образец по одной доступной странице ниже и выше 4 ГиБ.
/// Из памяти не читаем и её не изменяем: используем только карту загрузчика.
fn select_ram_samples(info: &BootInfo) -> [Option<u64>; 2] {
    let mut samples = [None, None];
    for region in info.memory_regions.iter() {
        if region.kind != MemoryRegionKind::Usable {
            continue;
        }
        let Some(candidate) = region.start.max(0x10_0000).checked_add(4095) else {
            continue;
        };
        let aligned_start = candidate & !4095;
        let aligned_end = region.end & !4095;
        if aligned_start >= aligned_end {
            continue;
        }
        if aligned_start < 0x1_0000_0000 && samples[0].is_none() {
            samples[0] = Some(aligned_start);
        }
        let high_start = aligned_start.max(0x1_0000_0000);
        if high_start < aligned_end && samples[1].is_none() {
            samples[1] = Some(high_start);
        }
    }
    samples
}

struct ReadOnlyMemory {
    offset: u64,
    physical_end: u64,
}

impl ReadOnlyMemory {
    fn from_context(context: &HardwareContext) -> Result<Self, ReadError> {
        let offset = context.physical_offset.ok_or(ReadError::MissingMapping)?;
        Ok(Self {
            offset,
            physical_end: context.physical_end,
        })
    }

    fn bytes(&self, physical: u64, length: usize) -> Result<&'static [u8], ReadError> {
        let end = physical
            .checked_add(length as u64)
            .ok_or(ReadError::BadAddress)?;
        if length == 0 || end > self.physical_end {
            return Err(ReadError::BadAddress);
        }
        let virtual_address = self
            .offset
            .checked_add(physical)
            .ok_or(ReadError::BadAddress)?;
        let virtual_address =
            VirtAddr::try_new(virtual_address).map_err(|_| ReadError::BadAddress)?;
        // Загрузчик гарантирует непрерывное линейное отображение физического
        // адресного пространства; проверены границы и каноничность.
        Ok(unsafe { core::slice::from_raw_parts(virtual_address.as_ptr(), length) })
    }

    fn table(&self, physical: u64) -> Result<&'static [u8], ReadError> {
        let prefix = self.bytes(physical, SDT_HEADER_SIZE)?;
        let header = SdtHeader::parse_prefix(prefix).map_err(|_| ReadError::InvalidAcpi)?;
        let size = header.length as usize;
        if size > MAX_ACPI_TABLE {
            return Err(ReadError::InvalidAcpi);
        }
        self.bytes(physical, size)
    }

    fn covers_ecam(&self, base: u64, end_bus: u8) -> bool {
        let size = (u64::from(end_bus) + 1) << 20;
        base.checked_add(size)
            .is_some_and(|end| end <= self.physical_end)
    }
}

struct DirectEcam<'a> {
    memory: &'a ReadOnlyMemory,
}

impl ConfigAccess for DirectEcam<'_> {
    fn read_u32(&self, physical_address: u64) -> u32 {
        let address = self
            .memory
            .offset
            .checked_add(physical_address)
            .and_then(|value| VirtAddr::try_new(value).ok().map(|v| v.as_u64()))
            .expect("ECAM вышел за проверенный адресный диапазон");
        // Чтение пространства конфигурации PCIe; запись не используется.
        unsafe { (address as *const u32).read_volatile() }
    }
}

pub fn run(context: &HardwareContext, out: &mut Report<'_>) {
    cpu(context, out);
    let Some(rsdp) = context.rsdp else {
        log!(out, "[WARN] diagnostic ACPI: no RSDP");
        return;
    };
    let memory = match ReadOnlyMemory::from_context(context) {
        Ok(memory) => memory,
        Err(error) => {
            log!(out, "[WARN] diagnostic physical mapping: {error:?}");
            return;
        }
    };
    acpi_and_pci(rsdp, &memory, out);
}

fn cpu(context: &HardwareContext, out: &mut Report<'_>) {
    let basic = __cpuid(0);
    let mut vendor = [0u8; 12];
    vendor[..4].copy_from_slice(&basic.ebx.to_le_bytes());
    vendor[4..8].copy_from_slice(&basic.edx.to_le_bytes());
    vendor[8..].copy_from_slice(&basic.ecx.to_le_bytes());
    let vendor = core::str::from_utf8(&vendor).unwrap_or("unknown");
    let leaf1 = __cpuid(1);
    let pat_supported = leaf1.edx & (1 << 16) != 0;
    let mtrr_supported = leaf1.edx & (1 << 12) != 0;
    let extended = __cpuid(0x8000_0000).eax;
    let physical_bits = if extended >= 0x8000_0008 {
        (__cpuid(0x8000_0008).eax & 0xff) as u8
    } else {
        0
    };
    log!(
        out,
        "[INFO] diagnostic CPU: OK vendor={} family_model={:08x} physical_bits={}",
        vendor,
        leaf1.eax,
        physical_bits
    );
    if !pat_supported || !mtrr_supported || physical_bits == 0 {
        log!(out, "[WARN] diagnostic cache: unsupported CPUID");
        return;
    }
    let pat = unsafe { read_msr(IA32_PAT) };
    let mtrr_cap = unsafe { read_msr(IA32_MTRRCAP) };
    let mtrr_default = unsafe { read_msr(IA32_MTRR_DEFAULT) };
    let count = (mtrr_cap & 0xff) as usize;
    log!(
        out,
        "[INFO] diagnostic cache registers: PAT={:016x} MTRRDEF={:016x} CAP={:016x}",
        pat,
        mtrr_default,
        mtrr_cap
    );
    if count > MAX_VARIABLE_MTRRS {
        log!(
            out,
            "[WARN] diagnostic cache: too many variable MTRRs {}",
            count
        );
        return;
    }
    let mut variable = [(0, 0); MAX_VARIABLE_MTRRS];
    for (index, item) in variable[..count].iter_mut().enumerate() {
        let register = 0x200 + index as u32 * 2;
        *item = (unsafe { read_msr(register) }, unsafe {
            read_msr(register + 1)
        });
    }
    let mut snapshot =
        match DmaCacheSnapshot::new(pat, mtrr_default, physical_bits, &variable[..count]) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                log!(out, "[WARN] diagnostic cache policy: {error:?}");
                return;
            }
        };
    if mtrr_default & (1 << 10) != 0 && mtrr_cap & (1 << 8) != 0 {
        let registers = [
            0x250, 0x258, 0x259, 0x268, 0x269, 0x26a, 0x26b, 0x26c, 0x26d, 0x26e, 0x26f,
        ];
        let mut fixed = [0u64; FIXED_MTRR_REGISTERS];
        for (slot, register) in fixed.iter_mut().zip(registers) {
            *slot = unsafe { read_msr(register) };
        }
        snapshot = snapshot.with_fixed(fixed);
    }
    log!(
        out,
        "[INFO] diagnostic cache: OK default={} variable={} fixed_enabled={}",
        memory_type_name(snapshot.default_memory_type()),
        snapshot.variable_count(),
        mtrr_default & (1 << 10) != 0
    );
    for sample in context.ram_samples.iter().flatten() {
        match snapshot.effective_mtrr_type(*sample) {
            Ok(kind) => log!(
                out,
                "[INFO] diagnostic cache sample: phys={:#x} MTRR={} PAT-alias=not-verified",
                sample,
                memory_type_name(kind)
            ),
            Err(error) => log!(
                out,
                "[WARN] diagnostic cache sample: phys={:#x} error={error:?}",
                sample
            ),
        }
    }
    log!(
        out,
        "[INFO] diagnostic IOMMU: table presence only, not enabled status"
    );
}

fn acpi_and_pci(rsdp_address: u64, memory: &ReadOnlyMemory, out: &mut Report<'_>) {
    let outcome = read_acpi(rsdp_address, memory, out);
    let Ok(mcfg) = outcome else {
        log!(out, "[WARN] diagnostic ACPI: {:?}", outcome.err());
        return;
    };
    let Some(mcfg) = mcfg else {
        log!(out, "[WARN] diagnostic PCI: MCFG absent");
        return;
    };

    let access = DirectEcam { memory };
    let mut total = 0u32;
    let mut storage = 0u32;
    let mut usb = 0u32;
    let mut network = 0u32;
    for entry in mcfg.iter() {
        if !memory.covers_ecam(entry.base_address, entry.end_bus) {
            log!(
                out,
                "[WARN] diagnostic PCI: ECAM unmapped group={}",
                entry.segment_group
            );
            continue;
        }
        let segment = match EcamSegment::new(
            entry.base_address,
            entry.segment_group,
            entry.start_bus,
            entry.end_bus,
        ) {
            Ok(segment) => segment,
            Err(_) => continue,
        };
        let result = phoenix_pcie::enumerate_segment(&access, segment, |function| {
            total = total.saturating_add(1);
            if function.class_code == 1 {
                storage = storage.saturating_add(1);
            }
            if function.class_code == 0xc && function.subclass == 3 {
                usb = usb.saturating_add(1);
            }
            if function.class_code == 2 {
                network = network.saturating_add(1);
            }
            if total <= 12 {
                log!(
                    out,
                    "[INFO] PCI {:02x}:{:02x}.{} {:04x}:{:04x} class={:02x}:{:02x}",
                    function.bus,
                    function.device,
                    function.function,
                    function.vendor_id,
                    function.device_id,
                    function.class_code,
                    function.subclass
                );
            }
        });
        if let Err(error) = result {
            log!(out, "[WARN] diagnostic PCI: enumeration {error:?}");
        }
    }
    log!(
        out,
        "[INFO] diagnostic PCI: OK functions={} storage={} USB={} network={}",
        total,
        storage,
        usb,
        network
    );
}

fn read_acpi(
    physical: u64,
    memory: &ReadOnlyMemory,
    out: &mut Report<'_>,
) -> Result<Option<McfgInfo>, ReadError> {
    let rsdp =
        Rsdp::parse(memory.bytes(physical, RSDP_V2_SIZE)?).map_err(|_| ReadError::InvalidAcpi)?;
    let xsdt_bytes = memory.table(rsdp.xsdt_address)?;
    let xsdt = Xsdt::parse(xsdt_bytes).map_err(|_| ReadError::InvalidAcpi)?;
    let mut mcfg = None;
    let mut iommu_kind = "none";
    for physical in xsdt.iter() {
        let bytes = memory.table(physical)?;
        let prefix = SdtHeader::parse_prefix(bytes).map_err(|_| ReadError::InvalidAcpi)?;
        if phoenix_acpi::checksum(bytes) != 0 {
            return Err(ReadError::InvalidAcpi);
        }
        match &prefix.signature {
            b"MCFG" => {
                mcfg = Some(phoenix_acpi::parse_mcfg(bytes).map_err(|_| ReadError::InvalidAcpi)?);
            }
            b"IVRS" => iommu_kind = "AMD-IVRS",
            b"DMAR" => iommu_kind = "Intel-DMAR",
            _ => {}
        }
    }
    log!(
        out,
        "[INFO] diagnostic ACPI: OK xsdt_entries={} mcfg={} iommu_table={}",
        xsdt.entry_count,
        mcfg.is_some(),
        iommu_kind
    );
    Ok(mcfg)
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

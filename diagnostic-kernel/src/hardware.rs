//! Пассивное обследование ЦП, PAT/MTRR, ACPI и PCIe.
//! Здесь нет распределителя DMA, драйверов устройств или записи в PCI/MSR.

use crate::screen::Report;
use bootloader_api::BootInfo;
use bootloader_api::info::MemoryRegionKind;
use core::arch::x86_64::{__cpuid, __cpuid_count};
use phoenix_acpi::iommu_observation::{ReadOnlyIommuState, validate_passive_read};
use phoenix_acpi::ivhd_scope::IvhdScopes;
use phoenix_acpi::{McfgInfo, RSDP_V2_SIZE, Rsdp, SDT_HEADER_SIZE, SdtHeader, Xsdt};
use phoenix_pcie::{ConfigAccess, EcamSegment};
use phoenix_vm::ActivePageTable;
use phoenix_vm::dma_cache::{
    DmaCacheSnapshot, FIXED_MTRR_REGISTERS, MAX_VARIABLE_MTRRS, memory_type_name,
};
use phoenix_vm::hardware_gate::{DmaSafetyEvidence, blocking_reason, diagnostic_dma_authorized};
use phoenix_vm::page_walk::{AliasAuditReport, walk_current_cr3};
use phoenix_vm::ram_audit::{ObservedMapping, RamAuditReport};
use x86_64::VirtAddr;
use x86_64::registers::control::Cr3;

const PHYSICAL_MAPPING_MINIMUM: u64 = 0x1_0000_0000;
const MAX_ACPI_TABLE: usize = 1024 * 1024;
const IA32_MTRRCAP: u32 = 0xfe;
const IA32_PAT: u32 = 0x277;
const IA32_MTRR_DEFAULT: u32 = 0x2ff;
const AMD_SYSCFG: u32 = 0xc001_0010;
const AMD_TOP_MEM2: u32 = 0xc001_001d;
const AMD_TOM2_ENABLE: u64 = 1 << 21;
const AMD_TOM2_FORCE_WB: u64 = 1 << 22;
const MIN_AUDITED_RAM: u64 = 0x10_0000;
const MAX_RAM_REGIONS: usize = 256;

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
    ram_regions: [(u64, u64); MAX_RAM_REGIONS],
    ram_region_count: usize,
    ram_map_invalid: bool,
    memory_entries: u32,
    nonusable_entries: u32,
    usable_bytes: u64,
    high_usable_bytes: u64,
}

impl HardwareContext {
    pub fn from_boot_info(info: &BootInfo) -> Self {
        let mut ram_regions = [(0, 0); MAX_RAM_REGIONS];
        let mut ram_region_count = 0;
        let mut ram_map_invalid = false;
        let mut memory_entries = 0u32;
        let mut nonusable_entries = 0u32;
        let mut usable_bytes = 0u64;
        let mut high_usable_bytes = 0u64;

        for region in info.memory_regions.iter() {
            memory_entries = memory_entries.saturating_add(1);
            if region.kind != MemoryRegionKind::Usable {
                nonusable_entries = nonusable_entries.saturating_add(1);
                continue;
            }
            if region.end < region.start {
                ram_map_invalid = true;
                continue;
            }
            usable_bytes = usable_bytes.saturating_add(region.end - region.start);
            if region.end > 0x1_0000_0000 {
                high_usable_bytes =
                    high_usable_bytes.saturating_add(region.end - region.start.max(0x1_0000_0000));
            }
            let start = region.start.max(MIN_AUDITED_RAM);
            let end = region.end;
            if end <= MIN_AUDITED_RAM {
                continue;
            }
            if start >= end || start % 4096 != 0 || end % 4096 != 0 {
                ram_map_invalid = true;
                continue;
            }
            if ram_region_count == MAX_RAM_REGIONS {
                ram_map_invalid = true;
                continue;
            }
            let overlaps = ram_regions[..ram_region_count]
                .iter()
                .any(|&(a, b)| start < b && a < end);
            if overlaps {
                ram_map_invalid = true;
                continue;
            }
            ram_regions[ram_region_count] = (start, end);
            ram_region_count += 1;
        }

        Self {
            rsdp: info.rsdp_addr.into_option(),
            physical_offset: info.physical_memory_offset.into_option(),
            physical_end: info
                .memory_regions
                .iter()
                .fold(PHYSICAL_MAPPING_MINIMUM, |end, region| end.max(region.end)),
            ram_samples: select_ram_samples(info),
            ram_regions,
            ram_region_count,
            ram_map_invalid,
            memory_entries,
            nonusable_entries,
            usable_bytes,
            high_usable_bytes,
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
    log!(
        out,
        "[INFO] diagnostic memory map: entries={} RAM={} other={} RAM_MiB={} high_MiB={} invalid={}",
        context.memory_entries,
        context.ram_region_count,
        context.nonusable_entries,
        context.usable_bytes >> 20,
        context.high_usable_bytes >> 20,
        context.ram_map_invalid
    );
    let cache = cpu(context, out);
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
    acpi_and_pci(rsdp, &memory, context, cache.as_ref(), out);
}

fn cpu(context: &HardwareContext, out: &mut Report<'_>) -> Option<DmaCacheSnapshot> {
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
        return None;
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
        return None;
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
                return None;
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
    // У виртуальных ЦП чтение AMD-специфических MSR может вызвать #GP.
    // Такой режим оставляем в консервативном варианте без TOM2-исключения.
    if vendor == "AuthenticAMD" && (leaf1.ecx & (1 << 31)) == 0 {
        let syscfg = unsafe { read_msr(AMD_SYSCFG) };
        let top = if syscfg & AMD_TOM2_ENABLE != 0 {
            unsafe { read_msr(AMD_TOP_MEM2) }
        } else {
            0
        };
        log!(
            out,
            "[INFO] diagnostic AMD cache: SYSCFG={:#x} TOP_MEM2={:#x} TOM2={} FWB={}",
            syscfg,
            top,
            syscfg & AMD_TOM2_ENABLE != 0,
            syscfg & AMD_TOM2_FORCE_WB != 0
        );
        snapshot = match snapshot.with_amd_top_mem2(syscfg, top) {
            Ok(value) => value,
            Err(error) => {
                log!(out, "[WARN] diagnostic AMD TOM2: {error:?}");
                return None;
            }
        };
    }

    log!(
        out,
        "[INFO] diagnostic cache: OK default={} variable={} fixed_enabled={}",
        memory_type_name(snapshot.default_memory_type()),
        snapshot.variable_count(),
        mtrr_default & (1 << 10) != 0
    );
    let table = context
        .physical_offset
        .and_then(|offset| VirtAddr::try_new(offset).ok())
        .map(|offset| {
            // Загрузчик передал единственное прямое отображение RAM,
            // изменяемого владельца таблиц в диагностическом ядре нет.
            // Используем только translate_addr и pat_index.
            unsafe { ActivePageTable::from_current(offset) }
        });
    for sample in context.ram_samples.iter().flatten() {
        report_memory_sample(context, &snapshot, table.as_ref(), *sample, out);
    }
    let direct_ready = audit_ram_regions(context, &snapshot, table.as_ref(), out);
    audit_current_aliases(context, &snapshot, table.as_ref(), direct_ready, out);
    log!(
        out,
        "[INFO] diagnostic IOMMU: table presence only, not enabled status"
    );
    Some(snapshot)
}

/// Сверка физического адреса с фактическим прямым отображением RAM.
/// Ничего не записываем в память и не выдаём новых DMA-отображений.
fn report_memory_sample(
    context: &HardwareContext,
    snapshot: &DmaCacheSnapshot,
    table: Option<&ActivePageTable>,
    physical: u64,
    out: &mut Report<'_>,
) {
    let kind = match snapshot.effective_mtrr_type(physical) {
        Ok(kind) => kind,
        Err(error) => {
            log!(
                out,
                "[WARN] diagnostic cache sample: phys={physical:#x} MTRR={error:?}"
            );
            return;
        }
    };
    let Some(table) = table else {
        log!(
            out,
            "[WARN] diagnostic cache sample: phys={physical:#x} PAT=no-table"
        );
        return;
    };
    let address = context
        .physical_offset
        .and_then(|offset| offset.checked_add(physical))
        .and_then(|address| VirtAddr::try_new(address).ok());
    let Some(address) = address else {
        log!(
            out,
            "[WARN] diagnostic cache sample: phys={physical:#x} PAT=bad-address"
        );
        return;
    };
    if table.translate_addr(address).map(|p| p.as_u64()) != Some(physical) {
        log!(
            out,
            "[WARN] diagnostic cache sample: phys={physical:#x} PAT=bad-mapping"
        );
        return;
    }
    let Some(index) = table.pat_index(address) else {
        log!(
            out,
            "[WARN] diagnostic cache sample: phys={physical:#x} PAT=unmapped"
        );
        return;
    };
    let pat_kind = pat_type_from_snapshot(snapshot, index);
    let candidate = snapshot.verify_writeback_alias(physical, 1, index).is_ok();
    log!(
        out,
        "[INFO] diagnostic cache sample: phys={physical:#x} MTRR={} PAT={} index={} WB-candidate={}",
        memory_type_name(kind),
        pat_kind,
        index,
        candidate
    );
}

fn pat_type_from_snapshot(snapshot: &DmaCacheSnapshot, index: u8) -> &'static str {
    snapshot
        .pat_memory_type(index)
        .map(memory_type_name)
        .unwrap_or("unknown")
}

/// Полный постраничный обход доступной RAM выше 1 МиБ, ограниченный
/// 16 ГиБ и 256 участками. Анализируется ТОЛЬКО прямое отображение.
/// Отсутствие других виртуальных псевдонимов эта проверка не доказывает.
fn audit_ram_regions(
    context: &HardwareContext,
    snapshot: &DmaCacheSnapshot,
    table: Option<&ActivePageTable>,
    out: &mut Report<'_>,
) -> bool {
    let Some(table) = table else {
        log!(
            out,
            "[WARN] diagnostic RAM audit: complete=false reason=no-page-table"
        );
        return false;
    };
    let Some(offset) = context.physical_offset else {
        log!(
            out,
            "[WARN] diagnostic RAM audit: complete=false reason=no-offset"
        );
        return false;
    };
    let mut report = RamAuditReport::default();
    let mut incomplete = context.ram_map_invalid || context.ram_region_count == 0;
    for &(start, end) in &context.ram_regions[..context.ram_region_count] {
        let result = report.inspect_region(snapshot, start, end, |physical| {
            let virtual_address = offset
                .checked_add(physical)
                .and_then(|raw| VirtAddr::try_new(raw).ok());
            let Some(virtual_address) = virtual_address else {
                return ObservedMapping::Missing;
            };
            let Some(mapped) = table.translate_addr(virtual_address) else {
                return ObservedMapping::Missing;
            };
            let Some(pat_index) = table.pat_index(virtual_address) else {
                return ObservedMapping::Missing;
            };
            ObservedMapping::Mapped {
                physical: mapped.as_u64(),
                pat_index,
            }
        });
        if result.is_err() {
            incomplete = true;
            break;
        }
    }

    let complete = !incomplete && report.invalid_mappings == 0;
    log!(
        out,
        "[INFO] diagnostic RAM audit: complete={} pages={} WB={} blocked={} invalid={} scope=direct-only",
        complete,
        report.scanned_pages,
        report.wb_candidates,
        report.blocked_pages,
        report.invalid_mappings
    );
    if let Some(physical) = report.first_invalid {
        log!(
            out,
            "[WARN] diagnostic RAM audit: first_invalid={physical:#x}"
        );
    }
    if let Some(physical) = report.first_blocked {
        log!(
            out,
            "[INFO] diagnostic RAM audit: first_blocked={physical:#x}"
        );
    }
    complete && report.blocked_pages == 0
}

/// Обход текущей CR3 без изменения таблиц и без обращения к MMIO.
/// Другие CR3, дополнительные процессоры и будущие отображения НЕ проверены.
fn audit_current_aliases(
    context: &HardwareContext,
    snapshot: &DmaCacheSnapshot,
    table: Option<&ActivePageTable>,
    direct_ready: bool,
    out: &mut Report<'_>,
) {
    let Some(table) = table else {
        log!(
            out,
            "[WARN] diagnostic aliases: complete=false reason=no-page-table"
        );
        return;
    };
    let Some(offset) = context.physical_offset else {
        log!(
            out,
            "[WARN] diagnostic aliases: complete=false reason=no-offset"
        );
        return;
    };

    let cr4: u64;
    unsafe {
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags))
    };
    if cr4 & (1 << 12) != 0 {
        log!(
            out,
            "[WARN] diagnostic aliases: complete=false reason=five-level-paging"
        );
        return;
    }

    let root = Cr3::read().0.start_address().as_u64();
    let mut result = AliasAuditReport::default();
    let mut alias_error = None;
    let mut last_checked = None;
    let scan = walk_current_cr3(
        root,
        snapshot.physical_bits(),
        |table_phys, index| {
            if table_phys.checked_add(4096)? > context.physical_end {
                return None;
            }
            if last_checked != Some(table_phys) {
                let virtual_table = offset
                    .checked_add(table_phys)
                    .and_then(|raw| VirtAddr::try_new(raw).ok())?;
                if table.translate_addr(virtual_table).map(|p| p.as_u64()) != Some(table_phys) {
                    return None;
                }
                last_checked = Some(table_phys);
            }
            let address = offset
                .checked_add(table_phys)?
                .checked_add(index as u64 * 8)
                .and_then(|raw| VirtAddr::try_new(raw).ok())?;
            // Только физические таблицы страниц текущего ядра,
            // предварительно проверена трансляция всей страницы.
            Some(unsafe { (address.as_u64() as *const u64).read_volatile() })
        },
        |leaf| {
            if alias_error.is_some() {
                return;
            }
            let outcome = result.inspect_leaf(
                leaf,
                offset,
                &context.ram_regions[..context.ram_region_count],
                snapshot,
                |physical| {
                    let virtual_addr = offset
                        .checked_add(physical)
                        .and_then(|raw| VirtAddr::try_new(raw).ok())?;
                    let mapped = table.translate_addr(virtual_addr)?.as_u64();
                    let pat = table.pat_index(virtual_addr)?;
                    Some((mapped, pat))
                },
            );
            if let Err(error) = outcome {
                alias_error = Some(error);
            }
        },
    );

    let complete = scan.is_ok()
        && alias_error.is_none()
        && !context.ram_map_invalid
        && result.conflicting_pages == 0
        && result.unavailable_pages == 0;
    let leaves = scan.as_ref().map(|v| v.leaf_mappings).unwrap_or(0);
    log!(
        out,
        "[INFO] diagnostic aliases: complete={} leaves={} other_pages={} conflicts={} unknown={} scope=current-cr3",
        complete,
        leaves,
        result.checked_pages,
        result.conflicting_pages,
        result.unavailable_pages
    );
    if let Some(physical) = result.first_conflict {
        log!(
            out,
            "[WARN] diagnostic aliases: first_conflict={physical:#x}"
        );
    }
    if let Some(physical) = result.first_unavailable {
        log!(
            out,
            "[WARN] diagnostic aliases: first_unknown={physical:#x}"
        );
    }
    if let Err(error) = scan {
        log!(out, "[WARN] diagnostic aliases: walk={error:?}");
    }
    if let Some(error) = alias_error {
        log!(out, "[WARN] diagnostic aliases: compare={error:?}");
    }

    // Ни текущая CR3, ни IVRS сами по себе не доказывают безопасность DMA.
    // Другие процессоры/таблицы, активная изоляция IOMMU и остановка
    // контроллера отсутствуют в диагностическом режиме.
    let evidence = DmaSafetyEvidence {
        memory_map_valid: !context.ram_map_invalid && context.ram_region_count > 0,
        direct_mapping_valid: direct_ready,
        current_cr3_aliases_clear: complete,
        ..DmaSafetyEvidence::default()
    };
    let reason = blocking_reason(&evidence);
    log!(
        out,
        "[INFO] diagnostic DMA gate: authorized={} reason={reason:?} readonly=1",
        diagnostic_dma_authorized(&evidence)
    );
}

fn acpi_and_pci(
    rsdp_address: u64,
    memory: &ReadOnlyMemory,
    context: &HardwareContext,
    cache: Option<&DmaCacheSnapshot>,
    out: &mut Report<'_>,
) {
    let outcome = read_acpi(rsdp_address, memory, context, cache, out);
    let Ok((mcfg, scopes)) = outcome else {
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
    let mut bus_master = 0u32;
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
            // Поле Command доступно для чтения: ничего не включаем
            // и не меняем в работе PCIe-контроллеров.
            let command = access.read_u32(function.config_address + 4) as u16;
            if command & 4 != 0 {
                bus_master = bus_master.saturating_add(1);
            }
            if function.class_code == 1 {
                storage = storage.saturating_add(1);
                let bdf = (u16::from(function.bus) << 8)
                    | (u16::from(function.device) << 3)
                    | u16::from(function.function);
                let scope = scopes
                    .as_ref()
                    .map(|map| map.query(entry.segment_group, bdf));
                log!(
                    out,
                    "[INFO] diagnostic PCI storage: {:02x}:{:02x}.{} vendor={:04x} device={:04x} iommu_scope={scope:?} protected=true DMA-approved=false",
                    function.bus,
                    function.device,
                    function.function,
                    function.vendor_id,
                    function.device_id
                );
            }
            if function.class_code == 0xc && function.subclass == 3 {
                usb = usb.saturating_add(1);
                let bdf = (u16::from(function.bus) << 8)
                    | (u16::from(function.device) << 3)
                    | u16::from(function.function);
                let scope = scopes
                    .as_ref()
                    .map(|map| map.query(entry.segment_group, bdf));
                log!(
                    out,
                    "[INFO] diagnostic PCI USB: {:02x}:{:02x}.{} vendor={:04x} device={:04x} iommu_scope={scope:?} protected=true DMA-approved=false",
                    function.bus,
                    function.device,
                    function.function,
                    function.vendor_id,
                    function.device_id
                );
            }
            if function.class_code == 2 {
                network = network.saturating_add(1);
                let bdf = (u16::from(function.bus) << 8)
                    | (u16::from(function.device) << 3)
                    | u16::from(function.function);
                let scope = scopes
                    .as_ref()
                    .map(|map| map.query(entry.segment_group, bdf));
                log!(
                    out,
                    "[INFO] diagnostic PCI network: {:02x}:{:02x}.{} vendor={:04x} device={:04x} busmaster={} iommu_scope={scope:?} DMA-approved=false",
                    function.bus,
                    function.device,
                    function.function,
                    function.vendor_id,
                    function.device_id,
                    command & 4 != 0
                );
            }
            if total <= 8 {
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
    log!(
        out,
        "[INFO] diagnostic PCI DMA: busmaster_enabled={} controller_selected=false write_commands=false",
        bus_master
    );
}

fn read_acpi(
    physical: u64,
    memory: &ReadOnlyMemory,
    context: &HardwareContext,
    cache: Option<&DmaCacheSnapshot>,
    out: &mut Report<'_>,
) -> Result<(Option<McfgInfo>, Option<IvhdScopes>), ReadError> {
    let rsdp =
        Rsdp::parse(memory.bytes(physical, RSDP_V2_SIZE)?).map_err(|_| ReadError::InvalidAcpi)?;
    let xsdt_bytes = memory.table(rsdp.xsdt_address)?;
    let xsdt = Xsdt::parse(xsdt_bytes).map_err(|_| ReadError::InvalidAcpi)?;
    let mut mcfg = None;
    let mut iommu_kind = "none";
    let mut ivrs_hardware = 0u16;
    let mut ivrs_invalid = false;
    let mut first_iommu_base = None;
    let mut device_scopes = None;
    let mut ivrs_tables = 0u8;
    let mut processor_count = None;
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
            b"IVRS" => {
                iommu_kind = "AMD-IVRS";
                ivrs_tables = ivrs_tables.saturating_add(1);
                match phoenix_acpi::ivrs::parse_ivrs(bytes) {
                    Ok(ivrs) => {
                        match phoenix_acpi::ivhd_scope::parse_ivhd_scopes(bytes) {
                            Ok(scopes) => {
                                log!(
                                    out,
                                    "[INFO] diagnostic IVHD scope: ranges={} unknown={} iommu_blocks={} domain=unverified",
                                    scopes.ranges_count(),
                                    scopes.unknown_entries,
                                    scopes.hardware_blocks
                                );
                                if ivrs_tables == 1 {
                                    device_scopes = Some(scopes);
                                } else {
                                    device_scopes = None;
                                }
                            }
                            Err(error) => log!(out, "[WARN] diagnostic IVHD scope: {error:?}"),
                        }
                        ivrs_hardware = ivrs.hardware_blocks;
                        first_iommu_base = ivrs.first_iommu_base;
                        log!(
                            out,
                            "[INFO] diagnostic IVRS: hardware={} reserved={} unknown={} remap_reported={} first_mmio={:?}",
                            ivrs.hardware_blocks,
                            ivrs.memory_reservations,
                            ivrs.unsupported_blocks,
                            ivrs.dma_remap_reported,
                            ivrs.first_iommu_base
                        );
                        log!(
                            out,
                            "[INFO] diagnostic IVMD: protected={} ambiguous_iommu={} device_scope=unverified",
                            ivrs.protected_count,
                            ivrs.ambiguous_iommu_base
                        );
                    }
                    Err(error) => {
                        ivrs_invalid = true;
                        log!(out, "[WARN] diagnostic IVRS: {error:?}");
                    }
                }
            }
            b"DMAR" => iommu_kind = "Intel-DMAR",
            b"APIC" => {
                match phoenix_acpi::parse_madt(bytes) {
                    Ok(madt) => processor_count = Some(madt.processor_count),
                    Err(error) => log!(out, "[WARN] diagnostic CPU topology: {error:?}"),
                }
                match phoenix_acpi::madt_topology::parse_madt_topology(bytes) {
                    Ok(topology) => {
                        log!(
                            out,
                            "[INFO] diagnostic CPU APIC IDs: enabled={} online_capable={} ids={:?} measured=false",
                            topology.enabled_count,
                            topology.online_capable_count,
                            topology.enabled_apic_ids()
                        );
                        report_bsp_coverage(&topology, cache.is_some(), out);
                    }
                    Err(error) => log!(
                        out,
                        "[WARN] diagnostic CPU APIC IDs: unverified reason={error:?}"
                    ),
                }
            }
            _ => {}
        }
    }
    if ivrs_tables > 1 {
        log!(
            out,
            "[WARN] diagnostic IVHD scope: multiple IVRS tables, device mapping incomplete"
        );
        device_scopes = None;
    }
    log!(
        out,
        "[INFO] diagnostic ACPI: OK xsdt_entries={} mcfg={} iommu_table={}",
        xsdt.entry_count,
        mcfg.is_some(),
        iommu_kind
    );
    log!(
        out,
        "[INFO] diagnostic IOMMU: ivhd={} structural_ok={} runtime=unverified domain=unverified",
        ivrs_hardware,
        !ivrs_invalid && ivrs_hardware > 0
    );
    passive_iommu_registers(first_iommu_base, memory, context, cache, out);
    log!(
        out,
        "[INFO] diagnostic CPU cores: MADT={processor_count:?} msr_consensus=unverified BSP_only=true"
    );
    Ok((mcfg, device_scopes))
}

/// Идентификатор непосредственно исполняющегося процессора по CPUID.
/// Идентификаторы из MADT сами по себе не означают, что другие ядра запущены.
fn running_apic_id() -> Option<(u32, &'static str)> {
    let basic_max = __cpuid(0).eax;
    for leaf in [0x1f, 0x0b] {
        if basic_max < leaf {
            continue;
        }
        let level = __cpuid_count(leaf, 0);
        // Нулевое число логических процессоров означает недействительный
        // уровень топологии, даже если числовой идентификатор равен нулю.
        if level.ebx & 0xffff != 0 && (level.ecx >> 8) & 0xff != 0 {
            return Some((level.edx, "x2apic-cpuid"));
        }
    }

    let max_extended = __cpuid(0x8000_0000).eax;
    if max_extended >= 0x8000_001e {
        let vendor = __cpuid(0);
        if vendor.ebx == 0x6874_7541 && vendor.edx == 0x6974_6e65 && vendor.ecx == 0x444d_4163 {
            return Some((__cpuid(0x8000_001e).eax, "amd-extended-cpuid"));
        }
    }

    let leaf1 = __cpuid(1);
    if leaf1.edx & (1 << 9) != 0 {
        return Some((leaf1.ebx >> 24, "legacy8-cpuid"));
    }
    None
}

/// Даже при верном измерении BSP остальные процессоры не проверены.
/// Снимок предназначен только для аудита, не для аппаратного допуска DMA.
fn report_bsp_coverage(
    topology: &phoenix_acpi::madt_topology::MadtTopology,
    cache_measured: bool,
    out: &mut Report<'_>,
) {
    use phoenix_acpi::madt_topology::verify_observed_processors;

    let (apic, source) = match running_apic_id() {
        Some((id, source)) => (Some(id), source),
        None => (None, "unavailable"),
    };
    let cr3 = Cr3::read().0.start_address().as_u64();
    let cr3_valid = cr3 != 0 && cr3 & 4095 == 0;
    // CPUID.1 кодирует только восемь младших битов APIC.
    // При наличии расширенных идентификаторов считать такое измерение
    // полным недопустимо: могут существовать коллизии.
    let id_unambiguous =
        source != "legacy8-cpuid" || topology.enabled_apic_ids().iter().all(|id| *id < 256);
    let sample_valid = cache_measured && cr3_valid && id_unambiguous;
    let measured = if sample_valid { apic.as_slice() } else { &[] };
    let result = verify_observed_processors(topology, measured);
    match result {
        Ok(coverage) => log!(
            out,
            "[INFO] diagnostic CPU coverage: expected={} measured={} missing={} ids_complete={} msr_consensus=unverified BSP_only=true",
            coverage.expected,
            coverage.observed,
            coverage.missing,
            coverage.identifiers_complete
        ),
        Err(reason) => log!(
            out,
            "[WARN] diagnostic CPU coverage: expected={} measured=0 missing=unverified ids_complete=false reason={reason:?} BSP_only=true",
            topology.enabled_count
        ),
    }
    log!(
        out,
        "[INFO] diagnostic CPU BSP: apic={apic:?} source={source} cr3={cr3:#x} cache_measured={cache_measured} cr3_valid={cr3_valid} id_unambiguous={id_unambiguous}"
    );
}

/// Никаких записей MMIO: только два поля известного контроллера,
/// и лишь если существующее отображение явно UC по PAT и MTRR.
/// Наличие установленного IOMMUEn без проверки домена НЕ разрешает DMA.
fn passive_iommu_registers(
    base: Option<u64>,
    memory: &ReadOnlyMemory,
    context: &HardwareContext,
    cache: Option<&DmaCacheSnapshot>,
    out: &mut Report<'_>,
) {
    let Some(base) = base else {
        log!(
            out,
            "[INFO] diagnostic IOMMU MMIO: observed=false reason=no-ivhd domain=unverified"
        );
        return;
    };
    let page = base & !4095;
    let virt = memory
        .offset
        .checked_add(base)
        .and_then(|x| VirtAddr::try_new(x).ok());
    let translated = virt.and_then(|address| {
        let offset = VirtAddr::try_new(memory.offset).ok()?;
        let table = unsafe { ActivePageTable::from_current(offset) };
        table
            .translate_addr(address)
            .map(|physical| physical.as_u64())
    });
    let pat = virt.and_then(|address| {
        let offset = VirtAddr::try_new(memory.offset).ok()?;
        let table = unsafe { ActivePageTable::from_current(offset) };
        let index = table.pat_index(address)?;
        cache?.pat_memory_type(index)
    });
    let mtrr = cache.and_then(|snapshot| snapshot.effective_mtrr_type(page).ok());
    let checked = validate_passive_read(
        base,
        context.physical_end,
        translated,
        mtrr,
        pat,
        &context.ram_regions[..context.ram_region_count],
    );
    if let Err(reason) = checked {
        log!(
            out,
            "[INFO] diagnostic IOMMU MMIO: observed=false reason={reason:?} domain=unverified"
        );
        return;
    }
    let Some(address) = virt else {
        log!(
            out,
            "[INFO] diagnostic IOMMU MMIO: observed=false reason=no-mapping domain=unverified"
        );
        return;
    };
    // Два документированных R/O наблюдения через явно некэшируемое
    // отображение: AMD IOMMU MMIO +0 (таблица устройств), +0x18 (управление).
    let table = unsafe { (address.as_u64() as *const u64).read_volatile() };
    let control = unsafe { ((address.as_u64() + 0x18) as *const u64).read_volatile() };
    let observation = ReadOnlyIommuState::decode(control, table);
    log!(
        out,
        "[INFO] diagnostic IOMMU MMIO: observed=true enabled_bit={} device_table_nonzero={} domain=unverified readonly=1",
        observation.enabled_bit,
        observation.table_address_nonzero
    );
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

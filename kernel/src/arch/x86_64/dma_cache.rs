//! Диагностика PAT/MTRR: только чтение регистров, без изменения кэша.
//! Любая неизвестная конфигурация отклоняется на границе нового WB-алиаса.

use core::arch::x86_64::__cpuid;
use phoenix_vm::dma_cache::{DmaCacheError, DmaCacheSnapshot, MAX_VARIABLE_MTRRS};

const IA32_MTRRCAP: u32 = 0x00fe;
const IA32_PAT: u32 = 0x0277;
const IA32_MTRR_DEF_TYPE: u32 = 0x02ff;
const IA32_MTRR_PHYSBASE0: u32 = 0x0200;

pub fn snapshot() -> Result<DmaCacheSnapshot, DmaCacheError> {
    let features = __cpuid(1);
    // PAT: CPUID.1:EDX[16], MTRR: CPUID.1:EDX[12].
    if features.edx & (1 << 16) == 0 || features.edx & (1 << 12) == 0 {
        return Err(DmaCacheError::UnsupportedConfiguration);
    }
    if __cpuid(0x8000_0000).eax < 0x8000_0008 {
        return Err(DmaCacheError::UnsupportedConfiguration);
    }
    let physical_bits = (__cpuid(0x8000_0008).eax & 0xff) as u8;

    // Чтение MSR возможно только после проверки возможностей ЦП.
    // В MSR не записываем: изменение PAT/MTRR требует отдельной
    // остановки процессоров и безопасного переключения кэширования.
    let pat = unsafe { read_msr(IA32_PAT) };
    let cap = unsafe { read_msr(IA32_MTRRCAP) };
    let default_type = unsafe { read_msr(IA32_MTRR_DEF_TYPE) };
    let count = (cap & 0xff) as usize;
    if count > MAX_VARIABLE_MTRRS {
        return Err(DmaCacheError::InsufficientCapacity);
    }

    let mut variable = [(0, 0); MAX_VARIABLE_MTRRS];
    for (index, entry) in variable[..count].iter_mut().enumerate() {
        let address = IA32_MTRR_PHYSBASE0 + (index as u32) * 2;
        *entry = (unsafe { read_msr(address) }, unsafe { read_msr(address + 1) });
    }
    DmaCacheSnapshot::new(pat, default_type, physical_bits, &variable[..count])
}

unsafe fn read_msr(register: u32) -> u64 {
    let low: u32;
    let high: u32;
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

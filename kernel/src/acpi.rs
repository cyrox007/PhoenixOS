use phoenix_acpi::{AcpiError, MadtInfo, McfgInfo, Rsdp, SDT_HEADER_SIZE, SdtHeader, Xsdt};
use x86_64::VirtAddr;

const MAX_ACPI_TABLE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformAcpiError {
    Parse(AcpiError),
    AddressOverflow,
    TableTooLarge,
    MissingMadt,
    MissingMcfg,
}

impl From<AcpiError> for PlatformAcpiError {
    fn from(error: AcpiError) -> Self {
        Self::Parse(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformAcpi {
    pub madt: MadtInfo,
    pub mcfg: McfgInfo,
}

pub fn discover(
    rsdp_physical_address: u64,
    physical_memory_offset: VirtAddr,
) -> Result<PlatformAcpi, PlatformAcpiError> {
    let rsdp_bytes = mapped_bytes(
        rsdp_physical_address,
        phoenix_acpi::RSDP_V2_SIZE,
        physical_memory_offset,
    )?;
    let rsdp = Rsdp::parse(rsdp_bytes)?;
    let xsdt_bytes = mapped_table(rsdp.xsdt_address, physical_memory_offset)?;
    let xsdt = Xsdt::parse(xsdt_bytes)?;

    let mut madt = None;
    let mut mcfg = None;

    for table_address in xsdt.iter() {
        let prefix = mapped_bytes(table_address, SDT_HEADER_SIZE, physical_memory_offset)?;
        let header = SdtHeader::parse_prefix(prefix)?;

        match &header.signature {
            b"APIC" => {
                let table = mapped_table(table_address, physical_memory_offset)?;
                madt = Some(phoenix_acpi::parse_madt(table)?);
            }
            b"MCFG" => {
                let table = mapped_table(table_address, physical_memory_offset)?;
                mcfg = Some(phoenix_acpi::parse_mcfg(table)?);
            }
            _ => {}
        }
    }

    Ok(PlatformAcpi {
        madt: madt.ok_or(PlatformAcpiError::MissingMadt)?,
        mcfg: mcfg.ok_or(PlatformAcpiError::MissingMcfg)?,
    })
}

fn mapped_table(
    physical_address: u64,
    physical_memory_offset: VirtAddr,
) -> Result<&'static [u8], PlatformAcpiError> {
    let prefix = mapped_bytes(physical_address, SDT_HEADER_SIZE, physical_memory_offset)?;
    let header = SdtHeader::parse_prefix(prefix)?;
    let length = header.length as usize;
    if length > MAX_ACPI_TABLE_BYTES {
        return Err(PlatformAcpiError::TableTooLarge);
    }
    mapped_bytes(physical_address, length, physical_memory_offset)
}

fn mapped_bytes(
    physical_address: u64,
    length: usize,
    physical_memory_offset: VirtAddr,
) -> Result<&'static [u8], PlatformAcpiError> {
    let virtual_address = physical_memory_offset
        .as_u64()
        .checked_add(physical_address)
        .ok_or(PlatformAcpiError::AddressOverflow)?;
    let pointer = VirtAddr::new(virtual_address).as_ptr::<u8>();

    Ok(unsafe { core::slice::from_raw_parts(pointer, length) })
}

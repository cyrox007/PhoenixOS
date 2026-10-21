#![no_std]

pub const SDT_HEADER_SIZE: usize = 36;
pub const RSDP_V2_SIZE: usize = 36;
pub const XSDT_ENTRY_SIZE: usize = 8;
pub const MAX_XSDT_ENTRIES: usize = 64;
pub const MAX_MCFG_SEGMENTS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpiError {
    Truncated,
    InvalidSignature,
    InvalidChecksum,
    UnsupportedRevision,
    InvalidLength,
    MissingXsdt,
    TooManyEntries,
    InvalidMadtRecord,
    InvalidMcfgLength,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rsdp {
    pub revision: u8,
    pub xsdt_address: u64,
}

impl Rsdp {
    pub fn parse(bytes: &[u8]) -> Result<Self, AcpiError> {
        if bytes.len() < RSDP_V2_SIZE {
            return Err(AcpiError::Truncated);
        }
        if &bytes[..8] != b"RSD PTR " {
            return Err(AcpiError::InvalidSignature);
        }
        if checksum(&bytes[..20]) != 0 {
            return Err(AcpiError::InvalidChecksum);
        }

        let revision = bytes[15];
        if revision < 2 {
            return Err(AcpiError::UnsupportedRevision);
        }

        let length = read_u32(bytes, 20)? as usize;
        if length < RSDP_V2_SIZE || length > bytes.len() {
            return Err(AcpiError::InvalidLength);
        }
        if checksum(&bytes[..length]) != 0 {
            return Err(AcpiError::InvalidChecksum);
        }

        let xsdt_address = read_u64(bytes, 24)?;
        if xsdt_address == 0 {
            return Err(AcpiError::MissingXsdt);
        }

        Ok(Self {
            revision,
            xsdt_address,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SdtHeader {
    pub signature: [u8; 4],
    pub length: u32,
    pub revision: u8,
}

impl SdtHeader {
    pub fn parse_prefix(bytes: &[u8]) -> Result<Self, AcpiError> {
        if bytes.len() < SDT_HEADER_SIZE {
            return Err(AcpiError::Truncated);
        }

        let mut signature = [0_u8; 4];
        signature.copy_from_slice(&bytes[..4]);
        let length = read_u32(bytes, 4)?;
        if length < SDT_HEADER_SIZE as u32 {
            return Err(AcpiError::InvalidLength);
        }

        Ok(Self {
            signature,
            length,
            revision: bytes[8],
        })
    }

    pub fn validate_table(bytes: &[u8], expected: [u8; 4]) -> Result<Self, AcpiError> {
        let header = Self::parse_prefix(bytes)?;
        if header.signature != expected {
            return Err(AcpiError::InvalidSignature);
        }
        let length = header.length as usize;
        if length > bytes.len() {
            return Err(AcpiError::Truncated);
        }
        if checksum(&bytes[..length]) != 0 {
            return Err(AcpiError::InvalidChecksum);
        }
        Ok(header)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Xsdt {
    pub entries: [u64; MAX_XSDT_ENTRIES],
    pub entry_count: usize,
}

impl Xsdt {
    pub fn parse(bytes: &[u8]) -> Result<Self, AcpiError> {
        let header = SdtHeader::validate_table(bytes, *b"XSDT")?;
        let table = &bytes[..header.length as usize];
        let payload = &table[SDT_HEADER_SIZE..];
        if payload.len() % XSDT_ENTRY_SIZE != 0 {
            return Err(AcpiError::InvalidLength);
        }

        let entry_count = payload.len() / XSDT_ENTRY_SIZE;
        if entry_count > MAX_XSDT_ENTRIES {
            return Err(AcpiError::TooManyEntries);
        }

        let mut entries = [0_u64; MAX_XSDT_ENTRIES];
        for (index, chunk) in payload.chunks_exact(XSDT_ENTRY_SIZE).enumerate() {
            entries[index] =
                u64::from_le_bytes(chunk.try_into().map_err(|_| AcpiError::Truncated)?);
        }

        Ok(Self {
            entries,
            entry_count,
        })
    }

    pub fn iter(&self) -> impl Iterator<Item = u64> + '_ {
        self.entries[..self.entry_count].iter().copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MadtInfo {
    pub local_apic_address: u32,
    pub flags: u32,
    pub processor_count: u16,
    pub x2apic_processor_count: u16,
    pub io_apic_count: u16,
}

pub fn parse_madt(bytes: &[u8]) -> Result<MadtInfo, AcpiError> {
    let header = SdtHeader::validate_table(bytes, *b"APIC")?;
    let length = header.length as usize;
    if length < SDT_HEADER_SIZE + 8 {
        return Err(AcpiError::InvalidLength);
    }

    let local_apic_address = read_u32(bytes, SDT_HEADER_SIZE)?;
    let flags = read_u32(bytes, SDT_HEADER_SIZE + 4)?;
    let mut processor_count = 0_u16;
    let mut x2apic_processor_count = 0_u16;
    let mut io_apic_count = 0_u16;
    let mut offset = SDT_HEADER_SIZE + 8;

    while offset < length {
        if offset + 2 > length {
            return Err(AcpiError::InvalidMadtRecord);
        }
        let kind = bytes[offset];
        let record_length = bytes[offset + 1] as usize;
        if record_length < 2 || offset + record_length > length {
            return Err(AcpiError::InvalidMadtRecord);
        }

        match kind {
            0 if record_length >= 8 => {
                let record_flags = read_u32(bytes, offset + 4)?;
                if record_flags & 0x3 != 0 {
                    processor_count = processor_count.saturating_add(1);
                }
            }
            1 if record_length >= 12 => {
                io_apic_count = io_apic_count.saturating_add(1);
            }
            9 if record_length >= 16 => {
                let record_flags = read_u32(bytes, offset + 8)?;
                if record_flags & 0x3 != 0 {
                    processor_count = processor_count.saturating_add(1);
                    x2apic_processor_count = x2apic_processor_count.saturating_add(1);
                }
            }
            _ => {}
        }

        offset += record_length;
    }

    Ok(MadtInfo {
        local_apic_address,
        flags,
        processor_count,
        x2apic_processor_count,
        io_apic_count,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McfgSegment {
    pub base_address: u64,
    pub segment_group: u16,
    pub start_bus: u8,
    pub end_bus: u8,
}

const EMPTY_MCFG_SEGMENT: McfgSegment = McfgSegment {
    base_address: 0,
    segment_group: 0,
    start_bus: 0,
    end_bus: 0,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McfgInfo {
    pub segments: [McfgSegment; MAX_MCFG_SEGMENTS],
    pub segment_count: usize,
}

impl McfgInfo {
    pub fn iter(&self) -> impl Iterator<Item = McfgSegment> + '_ {
        self.segments[..self.segment_count].iter().copied()
    }
}

pub fn parse_mcfg(bytes: &[u8]) -> Result<McfgInfo, AcpiError> {
    let header = SdtHeader::validate_table(bytes, *b"MCFG")?;
    let length = header.length as usize;
    let entries_offset = SDT_HEADER_SIZE + 8;
    if length < entries_offset || (length - entries_offset) % 16 != 0 {
        return Err(AcpiError::InvalidMcfgLength);
    }

    let segment_count = (length - entries_offset) / 16;
    if segment_count > MAX_MCFG_SEGMENTS {
        return Err(AcpiError::TooManyEntries);
    }

    let mut segments = [EMPTY_MCFG_SEGMENT; MAX_MCFG_SEGMENTS];
    for (index, segment) in segments[..segment_count].iter_mut().enumerate() {
        let offset = entries_offset + index * 16;
        let base_address = read_u64(bytes, offset)?;
        let segment_group = read_u16(bytes, offset + 8)?;
        let start_bus = bytes[offset + 10];
        let end_bus = bytes[offset + 11];
        if base_address == 0 || start_bus > end_bus {
            return Err(AcpiError::InvalidMcfgLength);
        }
        *segment = McfgSegment {
            base_address,
            segment_group,
            start_bus,
            end_bus,
        };
    }

    Ok(McfgInfo {
        segments,
        segment_count,
    })
}

pub const fn checksum(bytes: &[u8]) -> u8 {
    let mut sum = 0_u8;
    let mut index = 0;
    while index < bytes.len() {
        sum = sum.wrapping_add(bytes[index]);
        index += 1;
    }
    sum
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, AcpiError> {
    let end = offset.checked_add(2).ok_or(AcpiError::Truncated)?;
    let value = bytes.get(offset..end).ok_or(AcpiError::Truncated)?;
    Ok(u16::from_le_bytes(
        value.try_into().map_err(|_| AcpiError::Truncated)?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, AcpiError> {
    let end = offset.checked_add(4).ok_or(AcpiError::Truncated)?;
    let value = bytes.get(offset..end).ok_or(AcpiError::Truncated)?;
    Ok(u32::from_le_bytes(
        value.try_into().map_err(|_| AcpiError::Truncated)?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, AcpiError> {
    let end = offset.checked_add(8).ok_or(AcpiError::Truncated)?;
    let value = bytes.get(offset..end).ok_or(AcpiError::Truncated)?;
    Ok(u64::from_le_bytes(
        value.try_into().map_err(|_| AcpiError::Truncated)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seal(bytes: &mut [u8], checksum_offset: usize) {
        bytes[checksum_offset] = 0;
        bytes[checksum_offset] = 0_u8.wrapping_sub(checksum(bytes));
    }

    #[test]
    fn parses_rsdp_v2_and_rejects_damage() {
        let mut rsdp = [0_u8; RSDP_V2_SIZE];
        rsdp[..8].copy_from_slice(b"RSD PTR ");
        rsdp[15] = 2;
        rsdp[20..24].copy_from_slice(&(RSDP_V2_SIZE as u32).to_le_bytes());
        rsdp[24..32].copy_from_slice(&0x1234_5000_u64.to_le_bytes());
        seal(&mut rsdp[..20], 8);
        seal(&mut rsdp, 32);

        assert_eq!(
            Rsdp::parse(&rsdp),
            Ok(Rsdp {
                revision: 2,
                xsdt_address: 0x1234_5000
            })
        );

        rsdp[5] ^= 1;
        assert_eq!(Rsdp::parse(&rsdp), Err(AcpiError::InvalidSignature));
    }

    #[test]
    fn parses_xsdt_entries() {
        let mut table = [0_u8; SDT_HEADER_SIZE + 16];
        table[..4].copy_from_slice(b"XSDT");
        let length = table.len() as u32;
        table[4..8].copy_from_slice(&length.to_le_bytes());
        table[SDT_HEADER_SIZE..SDT_HEADER_SIZE + 8].copy_from_slice(&0x1000_u64.to_le_bytes());
        table[SDT_HEADER_SIZE + 8..].copy_from_slice(&0x2000_u64.to_le_bytes());
        seal(&mut table, 9);

        let xsdt = Xsdt::parse(&table).unwrap();
        assert_eq!(xsdt.entry_count, 2);
        assert_eq!(xsdt.entries[0], 0x1000);
        assert_eq!(xsdt.entries[1], 0x2000);
    }

    #[test]
    fn parses_madt_processor_and_io_apic_records() {
        let mut table = [0_u8; SDT_HEADER_SIZE + 8 + 8 + 12];
        table[..4].copy_from_slice(b"APIC");
        let length = table.len() as u32;
        table[4..8].copy_from_slice(&length.to_le_bytes());
        table[SDT_HEADER_SIZE..SDT_HEADER_SIZE + 4].copy_from_slice(&0xfee0_0000_u32.to_le_bytes());

        let cpu = SDT_HEADER_SIZE + 8;
        table[cpu] = 0;
        table[cpu + 1] = 8;
        table[cpu + 4..cpu + 8].copy_from_slice(&1_u32.to_le_bytes());

        let io = cpu + 8;
        table[io] = 1;
        table[io + 1] = 12;
        seal(&mut table, 9);

        let madt = parse_madt(&table).unwrap();
        assert_eq!(madt.processor_count, 1);
        assert_eq!(madt.io_apic_count, 1);
        assert_eq!(madt.local_apic_address, 0xfee0_0000);
    }

    #[test]
    fn parses_mcfg_segment() {
        let mut table = [0_u8; SDT_HEADER_SIZE + 8 + 16];
        table[..4].copy_from_slice(b"MCFG");
        let length = table.len() as u32;
        table[4..8].copy_from_slice(&length.to_le_bytes());
        let entry = SDT_HEADER_SIZE + 8;
        table[entry..entry + 8].copy_from_slice(&0xe000_0000_u64.to_le_bytes());
        table[entry + 10] = 0;
        table[entry + 11] = 0xff;
        seal(&mut table, 9);

        let mcfg = parse_mcfg(&table).unwrap();
        assert_eq!(mcfg.segment_count, 1);
        assert_eq!(mcfg.segments[0].base_address, 0xe000_0000);
        assert_eq!(mcfg.segments[0].end_bus, 0xff);
    }
}

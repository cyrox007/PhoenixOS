#![no_std]

const ELF_MAGIC: &[u8; 4] = b"\x7fELF";
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const EV_CURRENT: u8 = 1;
const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const EM_X86_64: u16 = 62;
const ELF64_HEADER_SIZE: usize = 64;
const ELF64_PROGRAM_HEADER_SIZE: usize = 56;
const PT_LOAD: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfError {
    HeaderTooSmall,
    InvalidMagic,
    UnsupportedClass,
    UnsupportedEndianness,
    UnsupportedVersion,
    UnsupportedType,
    UnsupportedMachine,
    InvalidHeaderSize,
    InvalidProgramHeaderSize,
    ProgramHeaderTableOutOfBounds,
    ProgramHeaderOutOfBounds,
    InvalidLoadSegment,
    InvalidAlignment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfType {
    Executable,
    PositionIndependent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentFlags(u32);

impl SegmentFlags {
    const EXECUTE: u32 = 1;
    const WRITE: u32 = 2;
    const READ: u32 = 4;

    pub const fn readable(self) -> bool {
        self.0 & Self::READ != 0
    }

    pub const fn writable(self) -> bool {
        self.0 & Self::WRITE != 0
    }

    pub const fn executable(self) -> bool {
        self.0 & Self::EXECUTE != 0
    }

    pub const fn bits(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadSegment<'a> {
    pub virtual_address: u64,
    pub memory_size: u64,
    pub alignment: u64,
    pub flags: SegmentFlags,
    file_offset: u64,
    file_bytes: &'a [u8],
}

impl<'a> LoadSegment<'a> {
    pub const fn file_offset(self) -> u64 {
        self.file_offset
    }

    pub const fn file_size(self) -> u64 {
        self.file_bytes.len() as u64
    }

    pub const fn file_bytes(self) -> &'a [u8] {
        self.file_bytes
    }
}

pub struct ElfImage<'a> {
    bytes: &'a [u8],
    elf_type: ElfType,
    entry_point: u64,
    program_headers_offset: u64,
    program_header_count: u16,
}

impl<'a> ElfImage<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ElfError> {
        if bytes.len() < ELF64_HEADER_SIZE {
            return Err(ElfError::HeaderTooSmall);
        }
        if bytes.get(0..4) != Some(ELF_MAGIC.as_slice()) {
            return Err(ElfError::InvalidMagic);
        }
        if bytes[4] != ELFCLASS64 {
            return Err(ElfError::UnsupportedClass);
        }
        if bytes[5] != ELFDATA2LSB {
            return Err(ElfError::UnsupportedEndianness);
        }
        if bytes[6] != EV_CURRENT {
            return Err(ElfError::UnsupportedVersion);
        }

        let elf_type = match read_u16(bytes, 16)? {
            ET_EXEC => ElfType::Executable,
            ET_DYN => ElfType::PositionIndependent,
            _ => return Err(ElfError::UnsupportedType),
        };
        if read_u16(bytes, 18)? != EM_X86_64 {
            return Err(ElfError::UnsupportedMachine);
        }
        if read_u32(bytes, 20)? != u32::from(EV_CURRENT) {
            return Err(ElfError::UnsupportedVersion);
        }
        if usize::from(read_u16(bytes, 52)?) != ELF64_HEADER_SIZE {
            return Err(ElfError::InvalidHeaderSize);
        }
        if usize::from(read_u16(bytes, 54)?) != ELF64_PROGRAM_HEADER_SIZE {
            return Err(ElfError::InvalidProgramHeaderSize);
        }

        let program_headers_offset = read_u64(bytes, 32)?;
        let program_header_count = read_u16(bytes, 56)?;
        validate_program_header_table(bytes, program_headers_offset, program_header_count)?;

        Ok(Self {
            bytes,
            elf_type,
            entry_point: read_u64(bytes, 24)?,
            program_headers_offset,
            program_header_count,
        })
    }

    pub const fn elf_type(&self) -> ElfType {
        self.elf_type
    }

    pub const fn entry_point(&self) -> u64 {
        self.entry_point
    }

    pub const fn program_header_count(&self) -> u16 {
        self.program_header_count
    }

    pub fn load_segments(&self) -> LoadSegments<'a> {
        LoadSegments {
            image: self.bytes,
            table_offset: self.program_headers_offset,
            count: self.program_header_count,
            index: 0,
            failed: false,
        }
    }
}

pub struct LoadSegments<'a> {
    image: &'a [u8],
    table_offset: u64,
    count: u16,
    index: u16,
    failed: bool,
}

impl<'a> Iterator for LoadSegments<'a> {
    type Item = Result<LoadSegment<'a>, ElfError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }

        while self.index < self.count {
            let index = self.index;
            self.index += 1;

            let header_offset = match program_header_offset(self.table_offset, index) {
                Ok(offset) => offset,
                Err(error) => {
                    self.failed = true;
                    return Some(Err(error));
                }
            };
            let header = match self.image.get(header_offset..header_offset + ELF64_PROGRAM_HEADER_SIZE)
            {
                Some(header) => header,
                None => {
                    self.failed = true;
                    return Some(Err(ElfError::ProgramHeaderOutOfBounds));
                }
            };

            let segment_type = match read_u32(header, 0) {
                Ok(value) => value,
                Err(error) => {
                    self.failed = true;
                    return Some(Err(error));
                }
            };
            if segment_type != PT_LOAD {
                continue;
            }

            let segment = parse_load_segment(self.image, header);
            if segment.is_err() {
                self.failed = true;
            }
            return Some(segment);
        }

        None
    }
}

fn validate_program_header_table(
    bytes: &[u8],
    offset: u64,
    count: u16,
) -> Result<(), ElfError> {
    let table_size = u64::from(count)
        .checked_mul(ELF64_PROGRAM_HEADER_SIZE as u64)
        .ok_or(ElfError::ProgramHeaderTableOutOfBounds)?;
    let end = offset
        .checked_add(table_size)
        .ok_or(ElfError::ProgramHeaderTableOutOfBounds)?;
    if end > bytes.len() as u64 {
        return Err(ElfError::ProgramHeaderTableOutOfBounds);
    }
    Ok(())
}

fn program_header_offset(table_offset: u64, index: u16) -> Result<usize, ElfError> {
    let relative = u64::from(index)
        .checked_mul(ELF64_PROGRAM_HEADER_SIZE as u64)
        .ok_or(ElfError::ProgramHeaderOutOfBounds)?;
    let offset = table_offset
        .checked_add(relative)
        .ok_or(ElfError::ProgramHeaderOutOfBounds)?;
    usize::try_from(offset).map_err(|_| ElfError::ProgramHeaderOutOfBounds)
}

fn parse_load_segment<'a>(image: &'a [u8], header: &[u8]) -> Result<LoadSegment<'a>, ElfError> {
    let flags = SegmentFlags(read_u32(header, 4)?);
    let file_offset = read_u64(header, 8)?;
    let virtual_address = read_u64(header, 16)?;
    let file_size = read_u64(header, 32)?;
    let memory_size = read_u64(header, 40)?;
    let alignment = read_u64(header, 48)?;

    if file_size > memory_size {
        return Err(ElfError::InvalidLoadSegment);
    }
    virtual_address
        .checked_add(memory_size)
        .ok_or(ElfError::InvalidLoadSegment)?;
    validate_alignment(file_offset, virtual_address, alignment)?;

    let file_end = file_offset
        .checked_add(file_size)
        .ok_or(ElfError::InvalidLoadSegment)?;
    if file_end > image.len() as u64 {
        return Err(ElfError::InvalidLoadSegment);
    }

    let start = usize::try_from(file_offset).map_err(|_| ElfError::InvalidLoadSegment)?;
    let end = usize::try_from(file_end).map_err(|_| ElfError::InvalidLoadSegment)?;
    let file_bytes = image
        .get(start..end)
        .ok_or(ElfError::InvalidLoadSegment)?;

    Ok(LoadSegment {
        virtual_address,
        memory_size,
        alignment,
        flags,
        file_offset,
        file_bytes,
    })
}

fn validate_alignment(
    file_offset: u64,
    virtual_address: u64,
    alignment: u64,
) -> Result<(), ElfError> {
    if alignment <= 1 {
        return Ok(());
    }
    if !alignment.is_power_of_two() {
        return Err(ElfError::InvalidAlignment);
    }
    if file_offset % alignment != virtual_address % alignment {
        return Err(ElfError::InvalidAlignment);
    }
    Ok(())
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, ElfError> {
    let raw = bytes
        .get(offset..offset + 2)
        .ok_or(ElfError::ProgramHeaderOutOfBounds)?;
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, ElfError> {
    let raw = bytes
        .get(offset..offset + 4)
        .ok_or(ElfError::ProgramHeaderOutOfBounds)?;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, ElfError> {
    let raw = bytes
        .get(offset..offset + 8)
        .ok_or(ElfError::ProgramHeaderOutOfBounds)?;
    Ok(u64::from_le_bytes([
        raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMAGE_SIZE: usize = ELF64_HEADER_SIZE + ELF64_PROGRAM_HEADER_SIZE + 16;

    fn valid_image() -> [u8; IMAGE_SIZE] {
        let mut image = [0_u8; IMAGE_SIZE];
        image[0..4].copy_from_slice(ELF_MAGIC);
        image[4] = ELFCLASS64;
        image[5] = ELFDATA2LSB;
        image[6] = EV_CURRENT;
        image[16..18].copy_from_slice(&ET_EXEC.to_le_bytes());
        image[18..20].copy_from_slice(&EM_X86_64.to_le_bytes());
        image[20..24].copy_from_slice(&(EV_CURRENT as u32).to_le_bytes());
        image[24..32].copy_from_slice(&0x400000_u64.to_le_bytes());
        image[32..40].copy_from_slice(&(ELF64_HEADER_SIZE as u64).to_le_bytes());
        image[52..54].copy_from_slice(&(ELF64_HEADER_SIZE as u16).to_le_bytes());
        image[54..56].copy_from_slice(&(ELF64_PROGRAM_HEADER_SIZE as u16).to_le_bytes());
        image[56..58].copy_from_slice(&1_u16.to_le_bytes());

        let ph = ELF64_HEADER_SIZE;
        image[ph..ph + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
        image[ph + 4..ph + 8].copy_from_slice(&5_u32.to_le_bytes());
        image[ph + 8..ph + 16]
            .copy_from_slice(&((ELF64_HEADER_SIZE + ELF64_PROGRAM_HEADER_SIZE) as u64).to_le_bytes());
        image[ph + 16..ph + 24].copy_from_slice(&0x401000_u64.to_le_bytes());
        image[ph + 32..ph + 40].copy_from_slice(&16_u64.to_le_bytes());
        image[ph + 40..ph + 48].copy_from_slice(&32_u64.to_le_bytes());
        image[ph + 48..ph + 56].copy_from_slice(&1_u64.to_le_bytes());

        let data = ELF64_HEADER_SIZE + ELF64_PROGRAM_HEADER_SIZE;
        image[data..data + 4].copy_from_slice(&[0x90, 0x90, 0xc3, 0]);
        image
    }

    #[test]
    fn parses_x86_64_executable_and_load_segment() {
        let image = valid_image();
        let elf = ElfImage::parse(&image).unwrap();
        let segment = elf.load_segments().next().unwrap().unwrap();

        assert_eq!(elf.elf_type(), ElfType::Executable);
        assert_eq!(elf.entry_point(), 0x400000);
        assert_eq!(segment.virtual_address, 0x401000);
        assert_eq!(segment.file_size(), 16);
        assert_eq!(segment.memory_size, 32);
        assert!(segment.flags.readable());
        assert!(segment.flags.executable());
        assert!(!segment.flags.writable());
    }

    #[test]
    fn rejects_wrong_magic_and_machine() {
        let mut image = valid_image();
        image[0] = 0;
        assert_eq!(ElfImage::parse(&image).err(), Some(ElfError::InvalidMagic));

        let mut image = valid_image();
        image[18..20].copy_from_slice(&3_u16.to_le_bytes());
        assert_eq!(
            ElfImage::parse(&image).err(),
            Some(ElfError::UnsupportedMachine)
        );
    }

    #[test]
    fn rejects_truncated_program_header_table() {
        let mut image = valid_image();
        image[32..40].copy_from_slice(&((IMAGE_SIZE - 8) as u64).to_le_bytes());
        assert_eq!(
            ElfImage::parse(&image).err(),
            Some(ElfError::ProgramHeaderTableOutOfBounds)
        );
    }

    #[test]
    fn rejects_invalid_load_segment_sizes() {
        let mut image = valid_image();
        let ph = ELF64_HEADER_SIZE;
        image[ph + 32..ph + 40].copy_from_slice(&33_u64.to_le_bytes());
        image[ph + 40..ph + 48].copy_from_slice(&32_u64.to_le_bytes());

        let elf = ElfImage::parse(&image).unwrap();
        assert_eq!(
            elf.load_segments().next().unwrap().err(),
            Some(ElfError::InvalidLoadSegment)
        );
    }

    #[test]
    fn rejects_invalid_segment_alignment() {
        let mut image = valid_image();
        let ph = ELF64_HEADER_SIZE;
        image[ph + 48..ph + 56].copy_from_slice(&3_u64.to_le_bytes());

        let elf = ElfImage::parse(&image).unwrap();
        assert_eq!(
            elf.load_segments().next().unwrap().err(),
            Some(ElfError::InvalidAlignment)
        );
    }
}

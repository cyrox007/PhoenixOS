//! Проверка реального выделения физических страниц совместно с контрактом DMA.
//! Адреса процессора и устройства здесь имитируются, оборудование не используется.

use bootloader_api::info::{MemoryRegion, MemoryRegionKind};
use phoenix_dma::{
    CpuVirtualAddress, DeviceAddress, DmaError, DmaLayout,
    buffer::{DmaBuffer, DmaBufferPlatform},
    mapping::{DmaMappingBackend, PhysicalAddress},
};
use phoenix_memory::{FRAME_SIZE, SystemFrameAllocator};
use x86_64::{PhysAddr, structures::paging::{PhysFrame, Size4KiB}};

struct TestPlatform {
    frames: SystemFrameAllocator<8>,
    free_count: usize,
    device_unmap_count: usize,
    cpu_unmap_count: usize,
}

impl TestPlatform {
    fn new() -> Self {
        let regions = [MemoryRegion {
            start: 0x1000,
            end: 0x11000,
            kind: MemoryRegionKind::Usable,
        }];
        Self {
            frames: SystemFrameAllocator::from_regions(&regions).unwrap(),
            free_count: 0,
            device_unmap_count: 0,
            cpu_unmap_count: 0,
        }
    }
}

impl DmaMappingBackend for TestPlatform {
    fn map(
        &mut self,
        physical: PhysicalAddress,
        _layout: DmaLayout,
    ) -> Result<DeviceAddress, DmaError> {
        Ok(DeviceAddress::new(physical.value() + 0x20_0000))
    }

    fn unmap(&mut self, _device: DeviceAddress, _layout: DmaLayout) -> Result<(), DmaError> {
        self.device_unmap_count += 1;
        Ok(())
    }
}

impl DmaBufferPlatform for TestPlatform {
    fn reserve(&mut self, layout: DmaLayout) -> Result<PhysicalAddress, DmaError> {
        let bytes = u64::try_from(layout.length()).map_err(|_| DmaError::Unaddressable)?;
        let frames = bytes
            .checked_add(FRAME_SIZE - 1)
            .ok_or(DmaError::AddressOverflow)?
            / FRAME_SIZE;
        let alignment = u64::try_from(layout.alignment()).map_err(|_| DmaError::Unaddressable)?;
        let alignment_frames = (alignment / FRAME_SIZE).max(1);
        let frame = self
            .frames
            .allocate_contiguous_below_4k(frames, alignment_frames, 0xffff_ffff)
            .ok_or(DmaError::Unaddressable)?;
        Ok(PhysicalAddress::new(frame.start_address().as_u64()))
    }

    fn map_cpu(
        &mut self,
        physical: PhysicalAddress,
        _layout: DmaLayout,
    ) -> Result<CpuVirtualAddress, DmaError> {
        Ok(CpuVirtualAddress::new((physical.value() + 0x100_0000) as usize))
    }

    fn unmap_cpu(&mut self, _cpu: CpuVirtualAddress, _layout: DmaLayout) -> Result<(), DmaError> {
        self.cpu_unmap_count += 1;
        Ok(())
    }

    fn free(&mut self, physical: PhysicalAddress, layout: DmaLayout) -> Result<(), DmaError> {
        let count = (layout.length() as u64 + FRAME_SIZE - 1) / FRAME_SIZE;
        let first: PhysFrame<Size4KiB> =
            PhysFrame::containing_address(PhysAddr::new(physical.value()));
        self.frames
            .release_contiguous_4k(first, count)
            .map_err(|_| DmaError::InvalidRange)?;
        self.free_count += 1;
        Ok(())
    }
}

#[test]
fn physical_frames_remain_reserved_until_device_and_cpu_are_unmapped() {
    let mut platform = TestPlatform::new();
    let before = platform.frames.free_frames();
    let layout = DmaLayout::new(8192, 4096).unwrap();
    let mut buffer = DmaBuffer::reserve(&mut platform, layout).unwrap();
    assert_eq!(platform.frames.free_frames(), before - 2);
    buffer.map_cpu(&mut platform).unwrap();
    buffer.map_device(&mut platform).unwrap();
    assert_ne!(
        buffer.region().unwrap().cpu_address().value() as u64,
        buffer.region().unwrap().device_address().value()
    );
    buffer.begin_device_access().unwrap();
    assert!(buffer.close(&mut platform).is_err());
    assert_eq!(platform.frames.free_frames(), before - 2);
    buffer.finish_device_access().unwrap();
    buffer.close(&mut platform).unwrap();
    assert_eq!(platform.frames.free_frames(), before);
    assert_eq!(platform.device_unmap_count, 1);
    assert_eq!(platform.cpu_unmap_count, 1);
    assert_eq!(platform.free_count, 1);
}

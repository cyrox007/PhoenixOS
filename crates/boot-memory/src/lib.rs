#![no_std]

use bootloader_api::info::{MemoryRegion, MemoryRegionKind};
use x86_64::{
    PhysAddr,
    structures::paging::{FrameAllocator, FrameDeallocator, PhysFrame, Size4KiB},
};

pub const FRAME_SIZE: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemorySummary {
    pub region_count: usize,
    pub usable_region_count: usize,
    pub total_bytes: u64,
    pub usable_bytes: u64,
}

impl MemorySummary {
    pub fn from_regions(regions: &[MemoryRegion]) -> Self {
        let mut summary = Self {
            region_count: regions.len(),
            usable_region_count: 0,
            total_bytes: 0,
            usable_bytes: 0,
        };

        for region in regions {
            let bytes = region.end.saturating_sub(region.start);
            summary.total_bytes = summary.total_bytes.saturating_add(bytes);

            if region.kind == MemoryRegionKind::Usable {
                summary.usable_region_count += 1;
                summary.usable_bytes = summary.usable_bytes.saturating_add(bytes);
            }
        }

        summary
    }
}

/// Ранний последовательный распределитель физических страниц.
///
/// Он не требует кучи и подходит для самых первых стадий загрузки, но не умеет
/// возвращать страницы. После появления системного распределителя новый код не
/// должен создавать второй экземпляр над теми же областями памяти.
pub struct BootFrameAllocator<'a> {
    regions: &'a [MemoryRegion],
    region_index: usize,
    cursor: u64,
}

impl<'a> BootFrameAllocator<'a> {
    /// # Безопасность
    ///
    /// Вызывающая сторона обязана гарантировать единоличное право этого
    /// распределителя выдавать страницы из переданных свободных областей.
    pub unsafe fn new(regions: &'a [MemoryRegion]) -> Self {
        Self {
            regions,
            region_index: 0,
            cursor: 0,
        }
    }

    pub fn allocate_4k(&mut self) -> Option<PhysFrame<Size4KiB>> {
        loop {
            let region = self.regions.get(self.region_index)?;

            if region.kind != MemoryRegionKind::Usable {
                self.region_index += 1;
                self.cursor = 0;
                continue;
            }

            let candidate = align_up(self.cursor.max(region.start), FRAME_SIZE)?;

            if candidate.checked_add(FRAME_SIZE)? <= region.end {
                self.cursor = candidate + FRAME_SIZE;
                return Some(PhysFrame::containing_address(PhysAddr::new(candidate)));
            }

            self.region_index += 1;
            self.cursor = 0;
        }
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootFrameAllocator<'_> {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        self.allocate_4k()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalMemoryError {
    MetadataExhausted,
    FrameAlreadyFree,
    FrameOutsideManagedMemory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameRange {
    start: u64,
    end: u64,
}

impl FrameRange {
    const EMPTY: Self = Self { start: 0, end: 0 };

    fn from_memory_region(region: &MemoryRegion) -> Option<Self> {
        if region.kind != MemoryRegionKind::Usable {
            return None;
        }

        let start = div_ceil(region.start, FRAME_SIZE)?;
        let end = region.end / FRAME_SIZE;

        if start >= end {
            return None;
        }

        Some(Self { start, end })
    }

    fn contains(self, frame: u64) -> bool {
        self.start <= frame && frame < self.end
    }

    fn len(self) -> u64 {
        self.end.saturating_sub(self.start)
    }
}

/// Освобождающий системный распределитель физических страниц.
///
/// Свободная память хранится в виде упорядоченного набора диапазонов. Такая
/// схема не требует кучи и не ограничивает объём ОЗУ размером побитовой карты.
/// Цена ранней реализации — линейный поиск по числу свободных диапазонов.
///
/// `MAX_RANGES` ограничивает не объём памяти, а максимальную фрагментацию
/// списка свободных диапазонов. При исчерпании служебных записей освобождение
/// возвращает явную ошибку.
pub struct SystemFrameAllocator<const MAX_RANGES: usize> {
    managed: [FrameRange; MAX_RANGES],
    managed_len: usize,
    free: [FrameRange; MAX_RANGES],
    free_len: usize,
}

impl<const MAX_RANGES: usize> SystemFrameAllocator<MAX_RANGES> {
    /// Создаёт распределитель из областей, помеченных загрузчиком как свободные.
    ///
    /// Экземпляр должен быть единственным владельцем этих областей. Если до
    /// создания системного распределителя из них уже выдавались страницы,
    /// такие страницы необходимо отдельно зарезервировать либо сразу использовать
    /// системный распределитель как единственный источник физических страниц.
    pub fn from_regions(regions: &[MemoryRegion]) -> Result<Self, PhysicalMemoryError> {
        let mut allocator = Self {
            managed: [FrameRange::EMPTY; MAX_RANGES],
            managed_len: 0,
            free: [FrameRange::EMPTY; MAX_RANGES],
            free_len: 0,
        };

        for region in regions {
            let Some(range) = FrameRange::from_memory_region(region) else {
                continue;
            };

            insert_range(
                &mut allocator.managed,
                &mut allocator.managed_len,
                range,
            )?;
            insert_range(&mut allocator.free, &mut allocator.free_len, range)?;
        }

        Ok(allocator)
    }

    pub fn allocate_4k(&mut self) -> Option<PhysFrame<Size4KiB>> {
        let first = self.free.first_mut()?;

        if self.free_len == 0 {
            return None;
        }

        let frame_number = first.start;
        first.start += 1;

        if first.start == first.end {
            remove_range(&mut self.free, &mut self.free_len, 0);
        }

        Some(frame_from_number(frame_number))
    }

    pub fn release_4k(
        &mut self,
        frame: PhysFrame<Size4KiB>,
    ) -> Result<(), PhysicalMemoryError> {
        let frame_number = frame.start_address().as_u64() / FRAME_SIZE;

        if !contains_frame(&self.managed, self.managed_len, frame_number) {
            return Err(PhysicalMemoryError::FrameOutsideManagedMemory);
        }

        if contains_frame(&self.free, self.free_len, frame_number) {
            return Err(PhysicalMemoryError::FrameAlreadyFree);
        }

        let end = frame_number
            .checked_add(1)
            .ok_or(PhysicalMemoryError::FrameOutsideManagedMemory)?;

        insert_range(
            &mut self.free,
            &mut self.free_len,
            FrameRange {
                start: frame_number,
                end,
            },
        )
    }

    pub fn free_frames(&self) -> u64 {
        self.free[..self.free_len]
            .iter()
            .fold(0, |total, range| total.saturating_add(range.len()))
    }

    pub fn free_ranges(&self) -> usize {
        self.free_len
    }
}

unsafe impl<const MAX_RANGES: usize> FrameAllocator<Size4KiB>
    for SystemFrameAllocator<MAX_RANGES>
{
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        self.allocate_4k()
    }
}

impl<const MAX_RANGES: usize> FrameDeallocator<Size4KiB> for SystemFrameAllocator<MAX_RANGES> {
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame<Size4KiB>) {
        if let Err(error) = self.release_4k(frame) {
            panic!("ошибка возврата физической страницы: {error:?}");
        }
    }
}

fn contains_frame(ranges: &[FrameRange], len: usize, frame: u64) -> bool {
    ranges[..len].iter().any(|range| range.contains(frame))
}

fn insert_range<const N: usize>(
    ranges: &mut [FrameRange; N],
    len: &mut usize,
    range: FrameRange,
) -> Result<(), PhysicalMemoryError> {
    if range.start >= range.end {
        return Ok(());
    }

    let mut first = 0;
    while first < *len && ranges[first].end < range.start {
        first += 1;
    }

    let mut merged = range;
    let mut last = first;

    while last < *len && ranges[last].start <= merged.end {
        merged.start = merged.start.min(ranges[last].start);
        merged.end = merged.end.max(ranges[last].end);
        last += 1;
    }

    if first == last {
        return insert_new_range(ranges, len, first, merged);
    }

    ranges[first] = merged;
    compact_ranges(ranges, len, first + 1, last);
    Ok(())
}

fn insert_new_range<const N: usize>(
    ranges: &mut [FrameRange; N],
    len: &mut usize,
    index: usize,
    range: FrameRange,
) -> Result<(), PhysicalMemoryError> {
    if *len == N {
        return Err(PhysicalMemoryError::MetadataExhausted);
    }

    for position in (index..*len).rev() {
        ranges[position + 1] = ranges[position];
    }

    ranges[index] = range;
    *len += 1;
    Ok(())
}

fn compact_ranges<const N: usize>(
    ranges: &mut [FrameRange; N],
    len: &mut usize,
    destination: usize,
    source: usize,
) {
    let removed = source.saturating_sub(destination);
    if removed == 0 {
        return;
    }

    let mut read = source;
    let mut write = destination;

    while read < *len {
        ranges[write] = ranges[read];
        read += 1;
        write += 1;
    }

    let new_len = *len - removed;
    for slot in &mut ranges[new_len..*len] {
        *slot = FrameRange::EMPTY;
    }
    *len = new_len;
}

fn remove_range<const N: usize>(ranges: &mut [FrameRange; N], len: &mut usize, index: usize) {
    if index >= *len {
        return;
    }

    let mut position = index;
    while position + 1 < *len {
        ranges[position] = ranges[position + 1];
        position += 1;
    }

    *len -= 1;
    ranges[*len] = FrameRange::EMPTY;
}

fn frame_from_number(number: u64) -> PhysFrame<Size4KiB> {
    PhysFrame::containing_address(PhysAddr::new(number * FRAME_SIZE))
}

fn align_up(value: u64, alignment: u64) -> Option<u64> {
    debug_assert!(alignment.is_power_of_two());
    value
        .checked_add(alignment - 1)
        .map(|v| v & !(alignment - 1))
}

fn div_ceil(value: u64, divisor: u64) -> Option<u64> {
    value
        .checked_add(divisor - 1)
        .map(|adjusted| adjusted / divisor)
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_regions() {
        let regions = [
            MemoryRegion {
                start: 0,
                end: 0x1000,
                kind: MemoryRegionKind::Bootloader,
            },
            MemoryRegion {
                start: 0x1000,
                end: 0x9000,
                kind: MemoryRegionKind::Usable,
            },
        ];

        let summary = MemorySummary::from_regions(&regions);

        assert_eq!(summary.region_count, 2);
        assert_eq!(summary.usable_region_count, 1);
        assert_eq!(summary.total_bytes, 0x9000);
        assert_eq!(summary.usable_bytes, 0x8000);
    }

    #[test]
    fn boot_allocator_skips_reserved_and_aligns_frames() {
        let regions = [
            MemoryRegion {
                start: 0,
                end: 0x1800,
                kind: MemoryRegionKind::Bootloader,
            },
            MemoryRegion {
                start: 0x1801,
                end: 0x5000,
                kind: MemoryRegionKind::Usable,
            },
        ];

        let mut allocator = unsafe { BootFrameAllocator::new(&regions) };

        assert_eq!(
            allocator.allocate_4k().unwrap().start_address().as_u64(),
            0x2000
        );
        assert_eq!(
            allocator.allocate_4k().unwrap().start_address().as_u64(),
            0x3000
        );
        assert_eq!(
            allocator.allocate_4k().unwrap().start_address().as_u64(),
            0x4000
        );
        assert!(allocator.allocate_4k().is_none());
    }

    #[test]
    fn boot_allocator_moves_between_usable_regions() {
        let regions = [
            MemoryRegion {
                start: 0x1000,
                end: 0x2000,
                kind: MemoryRegionKind::Usable,
            },
            MemoryRegion {
                start: 0x2000,
                end: 0x3000,
                kind: MemoryRegionKind::Bootloader,
            },
            MemoryRegion {
                start: 0x8000,
                end: 0x9000,
                kind: MemoryRegionKind::Usable,
            },
        ];

        let mut allocator = unsafe { BootFrameAllocator::new(&regions) };

        assert_eq!(
            allocator.allocate_4k().unwrap().start_address().as_u64(),
            0x1000
        );
        assert_eq!(
            allocator.allocate_4k().unwrap().start_address().as_u64(),
            0x8000
        );
        assert!(allocator.allocate_4k().is_none());
    }

    #[test]
    fn system_allocator_allocates_releases_and_reuses_frames() {
        let regions = [MemoryRegion {
            start: 0x1000,
            end: 0x5000,
            kind: MemoryRegionKind::Usable,
        }];

        let mut allocator = SystemFrameAllocator::<8>::from_regions(&regions).unwrap();

        let first = allocator.allocate_4k().unwrap();
        let second = allocator.allocate_4k().unwrap();

        assert_eq!(first.start_address().as_u64(), 0x1000);
        assert_eq!(second.start_address().as_u64(), 0x2000);
        assert_eq!(allocator.free_frames(), 2);

        allocator.release_4k(first).unwrap();

        assert_eq!(allocator.free_frames(), 3);
        assert_eq!(
            allocator.allocate_4k().unwrap().start_address().as_u64(),
            0x1000
        );
    }

    #[test]
    fn system_allocator_merges_returned_neighbors() {
        let regions = [MemoryRegion {
            start: 0x1000,
            end: 0x6000,
            kind: MemoryRegionKind::Usable,
        }];

        let mut allocator = SystemFrameAllocator::<8>::from_regions(&regions).unwrap();

        let first = allocator.allocate_4k().unwrap();
        let second = allocator.allocate_4k().unwrap();
        let third = allocator.allocate_4k().unwrap();

        allocator.release_4k(second).unwrap();
        assert_eq!(allocator.free_ranges(), 2);

        allocator.release_4k(first).unwrap();
        assert_eq!(allocator.free_ranges(), 2);

        allocator.release_4k(third).unwrap();
        assert_eq!(allocator.free_ranges(), 1);
        assert_eq!(allocator.free_frames(), 5);
    }

    #[test]
    fn system_allocator_rejects_double_release() {
        let regions = [MemoryRegion {
            start: 0x1000,
            end: 0x3000,
            kind: MemoryRegionKind::Usable,
        }];

        let mut allocator = SystemFrameAllocator::<4>::from_regions(&regions).unwrap();
        let frame = allocator.allocate_4k().unwrap();

        allocator.release_4k(frame).unwrap();

        assert_eq!(
            allocator.release_4k(frame),
            Err(PhysicalMemoryError::FrameAlreadyFree)
        );
    }

    #[test]
    fn system_allocator_rejects_foreign_frame() {
        let regions = [MemoryRegion {
            start: 0x1000,
            end: 0x3000,
            kind: MemoryRegionKind::Usable,
        }];

        let mut allocator = SystemFrameAllocator::<4>::from_regions(&regions).unwrap();
        let foreign = PhysFrame::containing_address(PhysAddr::new(0x9000));

        assert_eq!(
            allocator.release_4k(foreign),
            Err(PhysicalMemoryError::FrameOutsideManagedMemory)
        );
    }

    #[test]
    fn adjacent_usable_regions_are_coalesced() {
        let regions = [
            MemoryRegion {
                start: 0x1000,
                end: 0x3000,
                kind: MemoryRegionKind::Usable,
            },
            MemoryRegion {
                start: 0x3000,
                end: 0x5000,
                kind: MemoryRegionKind::Usable,
            },
        ];

        let allocator = SystemFrameAllocator::<1>::from_regions(&regions).unwrap();

        assert_eq!(allocator.free_ranges(), 1);
        assert_eq!(allocator.free_frames(), 4);
    }

    #[test]
    fn reports_metadata_exhaustion_for_fragmented_map() {
        let regions = [
            MemoryRegion {
                start: 0x1000,
                end: 0x2000,
                kind: MemoryRegionKind::Usable,
            },
            MemoryRegion {
                start: 0x2000,
                end: 0x3000,
                kind: MemoryRegionKind::Bootloader,
            },
            MemoryRegion {
                start: 0x3000,
                end: 0x4000,
                kind: MemoryRegionKind::Usable,
            },
        ];

        assert!(matches!(
            SystemFrameAllocator::<1>::from_regions(&regions),
            Err(PhysicalMemoryError::MetadataExhausted)
        ));
    }
}

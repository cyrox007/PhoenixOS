//! Общий учёт виртуальных диапазонов ядра без кучи.
//!
//! Передача одного экземпляра между подсистемами исключает пересечения
//! программных резервирований. Аппаратную карту активных таблиц проверяет ядро.

use crate::PAGE_SIZE;
use crate::dma_window::{DmaPageAccess, DmaReservationError, DmaWindowHandle, DmaWindowRegistry};
use x86_64::VirtAddr;

const KERNEL_CANONICAL_START: u64 = 0xffff_8000_0000_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtualRangeError {
    InvalidRange,
    Overlap,
    Exhausted,
    StaleHandle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualRangeHandle {
    slot: usize,
    generation: u64,
}

#[derive(Clone, Copy)]
struct RangeSlot {
    start: u64,
    pages: u64,
    generation: u64,
    occupied: bool,
}

impl RangeSlot {
    const EMPTY: Self = Self {
        start: 0,
        pages: 0,
        generation: 0,
        occupied: false,
    };

    fn end_exclusive(self) -> u128 {
        u128::from(self.start) + u128::from(self.pages) * u128::from(PAGE_SIZE)
    }
}

/// Единая таблица резервирований внутри отдельно выделенного ядром диапазона.
/// Сам диапазон должен быть исключён из всех иных системных распределителей.
/// Обращения к таблице должны быть синхронизированы внешним владельцем.
pub struct KernelVirtualRanges<const N: usize> {
    start: u64,
    end_exclusive: u128,
    slots: [RangeSlot; N],
}

impl<const N: usize> KernelVirtualRanges<N> {
    pub fn new(start: u64, pages: u64) -> Result<Self, VirtualRangeError> {
        validate_range(start, pages)?;
        Ok(Self {
            start,
            end_exclusive: end_of(start, pages),
            slots: [RangeSlot::EMPTY; N],
        })
    }

    pub fn reserve_at(
        &mut self,
        start: u64,
        pages: u64,
    ) -> Result<VirtualRangeHandle, VirtualRangeError> {
        validate_range(start, pages)?;
        let end = end_of(start, pages);
        if start < self.start || end > self.end_exclusive {
            return Err(VirtualRangeError::InvalidRange);
        }
        if self.slots.iter().any(|slot| {
            slot.occupied
                && u128::from(start) < slot.end_exclusive()
                && u128::from(slot.start) < end
        }) {
            return Err(VirtualRangeError::Overlap);
        }
        let slot = self
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| !slot.occupied && slot.generation != u64::MAX)
            .ok_or(VirtualRangeError::Exhausted)?;
        let (index, slot) = slot;
        slot.start = start;
        slot.pages = pages;
        slot.generation += 1;
        slot.occupied = true;
        Ok(VirtualRangeHandle {
            slot: index,
            generation: slot.generation,
        })
    }

    /// Ищет первый свободный диапазон, пропуская занятые области целиком.
    pub fn reserve_first_fit(
        &mut self,
        pages: u64,
    ) -> Result<(VirtualRangeHandle, u64), VirtualRangeError> {
        if pages == 0 {
            return Err(VirtualRangeError::InvalidRange);
        }
        let bytes = u128::from(pages) * u128::from(PAGE_SIZE);
        let mut candidate = u128::from(self.start);
        loop {
            if candidate + bytes > self.end_exclusive {
                return Err(VirtualRangeError::Exhausted);
            }
            let mut next = None;
            for slot in &self.slots {
                if !slot.occupied {
                    continue;
                }
                if candidate < slot.end_exclusive() && u128::from(slot.start) < candidate + bytes {
                    let end = slot.end_exclusive();
                    next = Some(next.map_or(end, |current: u128| current.max(end)));
                }
            }
            let Some(end) = next else {
                let address =
                    u64::try_from(candidate).map_err(|_| VirtualRangeError::InvalidRange)?;
                let handle = self.reserve_at(address, pages)?;
                return Ok((handle, address));
            };
            candidate = end;
        }
    }

    pub fn range(&self, handle: VirtualRangeHandle) -> Result<(u64, u64), VirtualRangeError> {
        let slot = self
            .slots
            .get(handle.slot)
            .ok_or(VirtualRangeError::StaleHandle)?;
        if !slot.occupied || slot.generation != handle.generation {
            return Err(VirtualRangeError::StaleHandle);
        }
        Ok((slot.start, slot.pages))
    }

    pub fn release(&mut self, handle: VirtualRangeHandle) -> Result<(), VirtualRangeError> {
        self.range(handle)?;
        self.slots[handle.slot].occupied = false;
        Ok(())
    }
}

fn end_of(start: u64, pages: u64) -> u128 {
    u128::from(start) + u128::from(pages) * u128::from(PAGE_SIZE)
}

fn validate_range(start: u64, pages: u64) -> Result<(), VirtualRangeError> {
    if pages == 0 || start % PAGE_SIZE != 0 || start < KERNEL_CANONICAL_START {
        return Err(VirtualRangeError::InvalidRange);
    }
    let end = end_of(start, pages);
    if end > u128::from(u64::MAX) + 1 {
        return Err(VirtualRangeError::InvalidRange);
    }
    let last = u64::try_from(end - 1).map_err(|_| VirtualRangeError::InvalidRange)?;
    if VirtAddr::try_new(start).is_err() || VirtAddr::try_new(last).is_err() {
        return Err(VirtualRangeError::InvalidRange);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedDmaError {
    Virtual(VirtualRangeError),
    Dma(DmaReservationError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedDmaHandle {
    virtual_range: VirtualRangeHandle,
    dma: DmaWindowHandle,
}

/// Программный владелец DMA-окон. Все остальные подсистемы резервируют адреса
/// через ту же общую таблицу KernelVirtualRanges.
pub struct ManagedDmaWindows<const N: usize> {
    dma: DmaWindowRegistry<N>,
}

impl<const N: usize> ManagedDmaWindows<N> {
    pub const fn new() -> Self {
        Self {
            dma: DmaWindowRegistry::new(),
        }
    }

    pub fn reserve<const M: usize>(
        &mut self,
        ranges: &mut KernelVirtualRanges<M>,
        physical: u64,
        pages: u64,
    ) -> Result<ManagedDmaHandle, ManagedDmaError> {
        let (virtual_range, start) = ranges
            .reserve_first_fit(pages)
            .map_err(ManagedDmaError::Virtual)?;
        match self.dma.reserve(start, physical, pages) {
            Ok(dma) => Ok(ManagedDmaHandle { virtual_range, dma }),
            Err(error) => {
                // Резервирование диапазона ещё не создало отображений.
                ranges
                    .release(virtual_range)
                    .map_err(ManagedDmaError::Virtual)?;
                Err(ManagedDmaError::Dma(error))
            }
        }
    }

    pub fn map_all<A: DmaPageAccess>(
        &mut self,
        handle: ManagedDmaHandle,
        access: &mut A,
    ) -> Result<(), ManagedDmaError> {
        self.dma
            .map_all(handle.dma, access)
            .map_err(ManagedDmaError::Dma)
    }

    pub fn unmap_all<A: DmaPageAccess>(
        &mut self,
        handle: ManagedDmaHandle,
        access: &mut A,
    ) -> Result<(), ManagedDmaError> {
        self.dma
            .unmap_all(handle.dma, access)
            .map_err(ManagedDmaError::Dma)
    }

    pub fn release<const M: usize>(
        &mut self,
        ranges: &mut KernelVirtualRanges<M>,
        handle: ManagedDmaHandle,
    ) -> Result<(), ManagedDmaError> {
        // Сначала проверяем, что общее резервирование всё ещё действительно.
        ranges
            .range(handle.virtual_range)
            .map_err(ManagedDmaError::Virtual)?;
        self.dma.release(handle.dma).map_err(ManagedDmaError::Dma)?;
        ranges
            .release(handle.virtual_range)
            .map_err(ManagedDmaError::Virtual)
    }
}

impl<const N: usize> Default for ManagedDmaWindows<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dma_window::DmaWindowError;

    const BASE: u64 = 0xffff_a000_0000_0000;

    struct FakePageAccess {
        mapped: [bool; 5],
        fail_unmap_once: bool,
    }

    impl DmaPageAccess for FakePageAccess {
        fn map_page(
            &mut self,
            virtual_addr: u64,
            _physical_addr: u64,
        ) -> Result<(), DmaWindowError> {
            let index = ((virtual_addr - BASE) / PAGE_SIZE) as usize;
            self.mapped[index] = true;
            Ok(())
        }

        fn unmap_page(
            &mut self,
            virtual_addr: u64,
            _physical_addr: u64,
        ) -> Result<(), DmaWindowError> {
            if self.fail_unmap_once {
                self.fail_unmap_once = false;
                return Err(DmaWindowError::UnmappingFailed);
            }
            let index = ((virtual_addr - BASE) / PAGE_SIZE) as usize;
            self.mapped[index] = false;
            Ok(())
        }
    }

    #[test]
    fn shared_reservations_prevent_cross_subsystem_aliases() {
        let mut ranges = KernelVirtualRanges::<4>::new(BASE, 5).unwrap();
        let other = ranges.reserve_at(BASE, 1).unwrap();
        let mut dma = ManagedDmaWindows::<2>::new();
        let first = dma.reserve(&mut ranges, 0x2000, 2).unwrap();
        let second = dma.reserve(&mut ranges, 0x8000, 1).unwrap();
        assert!(ranges.reserve_at(BASE + PAGE_SIZE, 1).is_err());
        assert!(dma.reserve(&mut ranges, 0x2000, 1).is_err());
        let mut pages = FakePageAccess {
            mapped: [false; 5],
            fail_unmap_once: true,
        };
        dma.map_all(first, &mut pages).unwrap();
        assert_eq!(
            dma.release(&mut ranges, first),
            Err(ManagedDmaError::Dma(DmaReservationError::StillMapped))
        );
        assert!(dma.unmap_all(first, &mut pages).is_err());
        assert!(dma.release(&mut ranges, first).is_err());
        dma.unmap_all(first, &mut pages).unwrap();
        dma.release(&mut ranges, first).unwrap();
        assert!(dma.release(&mut ranges, first).is_err());
        dma.release(&mut ranges, second).unwrap();
        ranges.release(other).unwrap();
        let recycled = ranges.reserve_at(BASE, 1).unwrap();
        assert_ne!(recycled, other);
    }

    #[test]
    fn failed_dma_reservation_rolls_back_virtual_range() {
        let mut ranges = KernelVirtualRanges::<2>::new(BASE, 3).unwrap();
        let mut dma = ManagedDmaWindows::<1>::new();
        let first = dma.reserve(&mut ranges, 0x2000, 1).unwrap();
        assert_eq!(
            dma.reserve(&mut ranges, 0x2000, 1),
            Err(ManagedDmaError::Dma(DmaReservationError::PhysicalAlias))
        );
        assert!(ranges.reserve_at(BASE + PAGE_SIZE, 1).is_ok());
        dma.release(&mut ranges, first).unwrap();
    }

    #[test]
    fn highest_kernel_range_has_no_overflow() {
        let mut ranges = KernelVirtualRanges::<1>::new(0xffff_ffff_ffff_f000, 1).unwrap();
        let handle = ranges.reserve_at(0xffff_ffff_ffff_f000, 1).unwrap();
        assert_eq!(ranges.range(handle), Ok((0xffff_ffff_ffff_f000, 1)));
        ranges.release(handle).unwrap();
        assert!(KernelVirtualRanges::<1>::new(BASE, 0).is_err());
        assert!(KernelVirtualRanges::<1>::new(BASE + 1, 1).is_err());
    }
}

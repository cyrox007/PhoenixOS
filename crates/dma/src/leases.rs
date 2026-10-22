//! Учёт владения DMA-областями без динамического выделения памяти.
//! Физические страницы предоставляет отдельный аппаратный распределитель.

use crate::{DmaError, DmaRegion};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaHandle {
    slot: usize,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaLeaseError {
    Exhausted,
    StaleHandle,
    GenerationExhausted,
    Allocation(DmaError),
}

#[derive(Clone, Copy)]
struct Slot {
    region: Option<DmaRegion>,
    generation: u64,
}

/// Реализация отвечает за физическое выделение, закрепление страниц,
/// отображение для устройства и снятие отображения после остановки DMA.
pub trait DmaBacking {
    fn allocate(&mut self, layout: crate::DmaLayout) -> Result<DmaRegion, DmaError>;
    fn release(&mut self, region: DmaRegion);
}

/// Реестр эксклюзивного владения. Не является потокобезопасным:
/// вызывающая сторона обязана синхронизировать обращения.
pub struct DmaLeaseTable<const N: usize> {
    slots: [Slot; N],
}

impl<const N: usize> DmaLeaseTable<N> {
    pub const fn new() -> Self {
        Self {
            slots: [Slot {
                region: None,
                generation: 0,
            }; N],
        }
    }

    pub fn acquire<B: DmaBacking>(
        &mut self,
        backing: &mut B,
        layout: crate::DmaLayout,
    ) -> Result<DmaHandle, DmaLeaseError> {
        let slot = self.slots.iter().position(|entry| {
            entry.region.is_none() && entry.generation != u64::MAX
        });
        let slot = slot.ok_or(DmaLeaseError::Exhausted)?;
        let region = backing.allocate(layout).map_err(DmaLeaseError::Allocation)?;
        self.slots[slot].generation += 1;
        self.slots[slot].region = Some(region);
        Ok(DmaHandle {
            slot,
            generation: self.slots[slot].generation,
        })
    }

    pub fn region(&self, handle: DmaHandle) -> Result<DmaRegion, DmaLeaseError> {
        let slot = self.slots.get(handle.slot).ok_or(DmaLeaseError::StaleHandle)?;
        if slot.generation != handle.generation {
            return Err(DmaLeaseError::StaleHandle);
        }
        slot.region.ok_or(DmaLeaseError::StaleHandle)
    }

    /// Вызывать только после подтверждения остановки доступа устройства.
    pub fn release<B: DmaBacking>(
        &mut self,
        backing: &mut B,
        handle: DmaHandle,
    ) -> Result<(), DmaLeaseError> {
        let region = self.region(handle)?;
        self.slots[handle.slot].region = None;
        backing.release(region);
        Ok(())
    }
}

impl<const N: usize> Default for DmaLeaseTable<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CpuVirtualAddress, DeviceAddress, DmaLayout};
    struct FakeBacking {
        allocs: usize,
        frees: usize,
    }
    impl DmaBacking for FakeBacking {
        fn allocate(&mut self, layout: DmaLayout) -> Result<DmaRegion, DmaError> {
            self.allocs += 1;
            DmaRegion::new(
                CpuVirtualAddress::new(0x1000),
                DeviceAddress::new(0x2000),
                layout,
            )
        }
        fn release(&mut self, _region: DmaRegion) {
            self.frees += 1;
        }
    }

    #[test]
    fn rejects_duplicate_and_stale_release() {
        let mut table = DmaLeaseTable::<1>::new();
        let mut backing = FakeBacking { allocs: 0, frees: 0 };
        let layout = DmaLayout::new(256, 64).unwrap();
        let first = table.acquire(&mut backing, layout).unwrap();
        assert_eq!(table.acquire(&mut backing, layout), Err(DmaLeaseError::Exhausted));
        table.release(&mut backing, first).unwrap();
        assert_eq!(table.release(&mut backing, first), Err(DmaLeaseError::StaleHandle));
        let second = table.acquire(&mut backing, layout).unwrap();
        assert_ne!(first, second);
        assert_eq!(table.region(first), Err(DmaLeaseError::StaleHandle));
        table.release(&mut backing, second).unwrap();
        assert_eq!((backing.allocs, backing.frees), (2, 2));
    }

    #[test]
    fn zero_capacity_does_not_allocate() {
        let mut table = DmaLeaseTable::<0>::new();
        let mut backing = FakeBacking { allocs: 0, frees: 0 };
        let layout = DmaLayout::new(64, 16).unwrap();
        assert_eq!(table.acquire(&mut backing, layout), Err(DmaLeaseError::Exhausted));
        assert_eq!(backing.allocs, 0);
    }
}

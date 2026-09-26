use linked_list_allocator::LockedHeap;
use phoenix_memory::SystemFrameAllocator;
use phoenix_vm::ActivePageTable;
use x86_64::VirtAddr;
use x86_64::structures::paging::mapper::MapToError;
use x86_64::structures::paging::{Page, PageTableFlags, Size4KiB};

pub const HEAP_START: u64 = 0x0000_5000_0000_0000;
pub const HEAP_SIZE: usize = 1024 * 1024;

const PAGE_SIZE: usize = 4096;
const HEAP_PAGE_COUNT: usize = HEAP_SIZE / PAGE_SIZE;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

#[derive(Debug)]
pub enum HeapInitError {
    NoPhysicalFrame,
    Map(MapToError<Size4KiB>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeapStats {
    pub size: usize,
    pub used: usize,
    pub free: usize,
}

pub fn init<const MAX_RANGES: usize>(
    page_table: &mut ActivePageTable,
    frames: &mut SystemFrameAllocator<MAX_RANGES>,
) -> Result<HeapStats, HeapInitError> {
    let mut mapped_pages = 0;

    while mapped_pages < HEAP_PAGE_COUNT {
        let page = heap_page(mapped_pages);
        let Some(frame) = frames.allocate_4k() else {
            rollback(page_table, frames, mapped_pages);
            return Err(HeapInitError::NoPhysicalFrame);
        };

        let result = unsafe {
            page_table.map_4k(
                page,
                frame,
                PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
                frames,
            )
        };

        if let Err(error) = result {
            frames
                .release_4k(frame)
                .expect("не удалось вернуть кадр после ошибки отображения кучи");
            rollback(page_table, frames, mapped_pages);
            return Err(HeapInitError::Map(error));
        }

        mapped_pages += 1;
    }

    unsafe {
        ALLOCATOR.lock().init(HEAP_START as *mut u8, HEAP_SIZE);
    }

    Ok(stats())
}

pub fn stats() -> HeapStats {
    let allocator = ALLOCATOR.lock();

    HeapStats {
        size: allocator.size(),
        used: allocator.used(),
        free: allocator.free(),
    }
}

fn rollback<const MAX_RANGES: usize>(
    page_table: &mut ActivePageTable,
    frames: &mut SystemFrameAllocator<MAX_RANGES>,
    mut mapped_pages: usize,
) {
    while mapped_pages > 0 {
        mapped_pages -= 1;
        let page = heap_page(mapped_pages);

        let Ok(frame) = page_table.unmap_4k(page) else {
            continue;
        };

        let _ = frames.release_4k(frame);
    }
}

fn heap_page(index: usize) -> Page<Size4KiB> {
    let address = HEAP_START + (index * PAGE_SIZE) as u64;
    Page::<Size4KiB>::containing_address(VirtAddr::new(address))
}

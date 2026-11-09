//! Проверка реального отображения RAM через общий реестр DMA в ядре QEMU.
//!
//! Контроллер в этом испытании не участвует: проверяются распределитель,
//! активные таблицы страниц, владение виртуальным окном и обратный порядок
//! освобождения. Программно сверяются PAT/MTRR и WB-алиасы;
//! реальные устройства и изменение настроек кэша не проверяются.

use phoenix_dma::DmaLayout;
use phoenix_dma::mapping::{DeviceMapping, DirectDmaAperture, PhysicalAddress, QemuDirectDma};
use phoenix_memory::SystemFrameAllocator;
use phoenix_vm::ActivePageTable;
use phoenix_vm::dma_window::{ActiveDmaPageAccess, DmaCachePolicy};
use phoenix_vm::kernel_reservations::{KernelVirtualRanges, ManagedDmaWindows};
use x86_64::VirtAddr;

const DMA_VIRTUAL_START: u64 = 0xffff_a200_0000_0000;
const DMA_WINDOW_PAGES: u64 = 64;
const TEST_PAGES: u64 = 2;
const FIRST_VALUE: u64 = 0x5048_4f45_4e49_5844;
const SECOND_VALUE: u64 = 0x444d_415f_4f4b_4159;

/// Программный владелец выделенного окна виртуальной памяти ядра.
/// В текущем QEMU-профиле адреса не выделяются никакой другой подсистеме.
struct KernelDmaArena {
    ranges: KernelVirtualRanges<8>,
    windows: ManagedDmaWindows<8>,
}

impl KernelDmaArena {
    fn new(table: &ActivePageTable) -> Self {
        let ranges = KernelVirtualRanges::new(DMA_VIRTUAL_START, DMA_WINDOW_PAGES)
            .expect("не удалось зарезервировать диапазон виртуальных адресов DMA");

        // Контракт отдельной виртуальной области подкрепляется проверкой
        // фактических таблиц страниц, до первого физического отображения.
        for index in 0..DMA_WINDOW_PAGES {
            let address = VirtAddr::new(DMA_VIRTUAL_START + index * phoenix_vm::PAGE_SIZE);
            assert!(
                table.translate_addr(address).is_none(),
                "виртуальное окно DMA уже занято отображением ядра"
            );
        }

        Self {
            ranges,
            windows: ManagedDmaWindows::new(),
        }
    }

    fn verify<const N: usize>(
        &mut self,
        table: &mut ActivePageTable,
        frames: &mut SystemFrameAllocator<N>,
        physical_memory_offset: VirtAddr,
    ) -> u64 {
        let before = frames.free_frames();
        let first_frame = frames
            .allocate_contiguous_below_4k(TEST_PAGES, 1, u64::MAX)
            .expect("системный распределитель не смог выделить физические страницы DMA");
        let physical = first_frame.start_address().as_u64();

        // Перед созданием второго WB-алиаса сверяем аппаратные PAT/MTRR
        // и фактический индекс PAT линейного отображения загрузчика.
        // При неизвестном режиме — отказ до отображения страниц/запуска DMA.
        let cache = crate::arch::x86_64::dma_cache::snapshot()
            .expect("невозможно подтвердить аппаратную политику кэша DMA");
        for index in 0..TEST_PAGES {
            let offset = index * phoenix_vm::PAGE_SIZE;
            let page_physical = physical + offset;
            let alias_address = VirtAddr::new(
                physical_memory_offset
                    .as_u64()
                    .checked_add(page_physical)
                    .expect("переполнение линейного адреса памяти"),
            );
            assert_eq!(
                table.translate_addr(alias_address).map(|p| p.as_u64()),
                Some(page_physical),
                "прямое отображение RAM не соответствует физической странице"
            );
            let index = table.pat_index(alias_address)
                .expect("не найден индекс PAT прямого отображения RAM");
            cache.verify_writeback_alias(page_physical, 1, index)
                .expect("недопустимый тип кэширования или конфликт алиасов DMA");
        }


        // В проверочном профиле QEMU нет IOMMU; здесь проверяется только
        // адресная модель, не аппаратная передача DMA.
        let aperture =
            DirectDmaAperture::new(0, 0x1_0000_0000).expect("некорректное адресное окно QEMU");
        let mut backend = unsafe { QemuDirectDma::<2>::new(aperture) };
        let layout = DmaLayout::new((TEST_PAGES * phoenix_vm::PAGE_SIZE) as usize, 4096)
            .expect("некорректный размер DMA-области");
        let mut device_mapping =
            DeviceMapping::establish(&mut backend, PhysicalAddress::new(physical), layout)
                .expect("не удалось передать физический диапазон механизму прямой адресации");
        assert_eq!(
            device_mapping.device_address().value(),
            physical,
            "QEMU без IOMMU должен использовать гостевой физический адрес"
        );
        assert_eq!(backend.active_count(), 1);

        let handle = self
            .windows
            .reserve(&mut self.ranges, physical, TEST_PAGES)
            .expect("общий реестр не смог зарезервировать DMA-окно");
        let virtual_start = self
            .windows
            .virtual_start(&self.ranges, handle)
            .expect("DMA-окно не найдено в общем реестре");
        for index in 0..TEST_PAGES {
            let address = VirtAddr::new(virtual_start + index * phoenix_vm::PAGE_SIZE);
            assert!(
                table.translate_addr(address).is_none(),
                "страница DMA уже отображена до начала проверки"
            );
        }

        // Оба процессорских WB-отображения разрешены только после проверки
        // PAT/MTRR и индекса PAT прямого отображения. Реальное устройство,
        // IOMMU и аппаратная согласованность DMA не подтверждаются.
        {
            let mut access = unsafe {
                ActiveDmaPageAccess::with_cache_policy(table, frames, DmaCachePolicy::WriteBack)
            };
            self.windows
                .map_all(handle, &mut access)
                .expect("не удалось отобразить физические страницы DMA");
        }

        let values = [FIRST_VALUE, SECOND_VALUE];
        for (index, value) in values.into_iter().enumerate() {
            let offset = index as u64 * phoenix_vm::PAGE_SIZE;
            let virtual_address = VirtAddr::new(virtual_start + offset);
            let physical_address = physical + offset;
            assert_eq!(
                table
                    .translate_addr(virtual_address)
                    .map(|address| address.as_u64()),
                Some(physical_address),
                "DMA-окно указывает не на зарезервированную физическую страницу"
            );
            let mapped_pointer = virtual_address.as_mut_ptr::<u64>();
            let direct_pointer = VirtAddr::new(
                physical_memory_offset
                    .as_u64()
                    .checked_add(physical_address)
                    .expect("переполнение адреса линейного отображения физической памяти"),
            )
            .as_ptr::<u64>();
            unsafe { mapped_pointer.write_volatile(value) };
            assert_eq!(
                unsafe { direct_pointer.read_volatile() },
                value,
                "данные двух отображений RAM не совпадают"
            );
        }

        {
            let mut access = unsafe {
                ActiveDmaPageAccess::with_cache_policy(table, frames, DmaCachePolicy::WriteBack)
            };
            self.windows
                .unmap_all(handle, &mut access)
                .expect("не удалось снять все отображения DMA");
        }

        for index in 0..TEST_PAGES {
            let address = VirtAddr::new(virtual_start + index * phoenix_vm::PAGE_SIZE);
            assert!(
                table.translate_addr(address).is_none(),
                "после освобождения DMA-окна осталось отображение"
            );
        }

        // Устройство в испытании не запускалось, поэтому безопасно
        // снять отображение перед возвратом физических страниц.
        device_mapping
            .close(&mut backend)
            .expect("не удалось снять адресное отображение DMA");
        assert_eq!(backend.active_count(), 0);

        self.windows
            .release(&mut self.ranges, handle)
            .expect("не удалось вернуть виртуальное DMA-окно");
        let before_data_release = frames.free_frames();
        frames
            .release_contiguous_4k(first_frame, TEST_PAGES)
            .expect("не удалось вернуть физические страницы DMA");
        assert_eq!(
            frames.free_frames(),
            before_data_release + TEST_PAGES,
            "физические страницы DMA не возвращены распределителю"
        );
        // OffsetPageTable не удаляет пустые промежуточные таблицы P4/P3/P2.
        // Эти служебные кадры остаются собственностью ядра и должны
        // использоваться повторно, а не учитываться как утечка DMA-данных.
        assert!(
            frames.free_frames() <= before,
            "число свободных кадров превысило исходное значение"
        );
        TEST_PAGES
    }
}

/// Полный ранний цикл повторяется: второй проход не должен выделять
/// дополнительные промежуточные таблицы страниц для того же окна.
pub fn self_test<const N: usize>(
    table: &mut ActivePageTable,
    frames: &mut SystemFrameAllocator<N>,
    physical_memory_offset: VirtAddr,
) -> (u64, u64) {
    let before = frames.free_frames();
    let mut arena = KernelDmaArena::new(table);
    arena.verify(table, frames, physical_memory_offset);
    let after_first = frames.free_frames();
    arena.verify(table, frames, physical_memory_offset);
    assert_eq!(
        frames.free_frames(),
        after_first,
        "повторное использование DMA-окна расходует новые служебные кадры"
    );
    (TEST_PAGES, before - after_first)
}

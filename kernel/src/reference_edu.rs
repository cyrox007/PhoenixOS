//! Аппаратная проверка эталонного PCI-драйвера в QEMU.
//!
//! Вся работа ограничена виртуальным PCI EDU 1234:11e8. Действительный
//! DMA проходит через общий DmaBuffer и системный распределитель;
//! отдельный MSI-вектор вызывает обработчик без выделений и блокировок.

use core::sync::atomic::{AtomicU64, Ordering, fence};
use phoenix_dma::{
    CpuVirtualAddress, DeviceAddress, DmaError, DmaLayout,
    buffer::{DmaBuffer, DmaBufferError, DmaBufferPlatform},
    mapping::{DirectDmaAperture, DmaMappingBackend, PhysicalAddress, QemuDirectDma},
};
use phoenix_memory::SystemFrameAllocator;
use phoenix_pcie::PcieFunction;
use phoenix_reference_edu::{self as edu, ExampleError, InterruptMailbox, ProbeContext};
use phoenix_vm::{ActivePageTable, PAGE_SIZE};
use x86_64::{
    PhysAddr, VirtAddr,
    structures::{
        idt::InterruptStackFrame,
        paging::{Page, PageTableFlags, PhysFrame, Size4KiB},
    },
};

const MMIO_VIRTUAL_ADDRESS: u64 = 0xffff_ae00_0000_0000;
const POLL_LIMIT: usize = 10_000_000;
const MSI_CAPABILITY_ID: u8 = 5;
const MSI_MESSAGE_ADDRESS: u32 = 0xfee0_0000;
const PCI_COMMAND_OFFSET: u64 = 4;
const PCI_BAR0_OFFSET: u64 = 0x10;
const PCI_CAPABILITIES_OFFSET: u64 = 0x34;
const PCI_MEMORY_ENABLE: u16 = 1 << 1;
const PCI_BUS_MASTER: u16 = 1 << 2;
const PCI_INTX_DISABLE: u16 = 1 << 10;

static EDU_IRQ_ADDRESS: AtomicU64 = AtomicU64::new(0);
static EDU_INTERRUPTS: InterruptMailbox = InterruptMailbox::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceError {
    UnsupportedDevice,
    PciRegion,
    Mapping,
    InvalidIdentity,
    MissingMsi,
    InvalidMsi,
    InterruptTimeout,
    DmaTimeout,
    Dma(DmaBufferError),
    DmaAddress,
    UnexpectedData,
    DmaNotReleased,
    Registry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferenceReport {
    pub pci_bus: u8,
    pub pci_device: u8,
    pub verified_bytes: usize,
    pub irq_mask: u32,
    pub returned_dma_pages: u64,
    pub driver_name: &'static str,
}

struct PciConfig {
    physical_memory_offset: VirtAddr,
    function: PcieFunction,
}

impl PciConfig {
    fn read_u8(&self, offset: u64) -> u8 {
        let word = self.read_u32(offset & !3);
        (word >> ((offset & 3) * 8)) as u8
    }

    fn read_u16(&self, offset: u64) -> u16 {
        assert_eq!(offset & 1, 0);
        let word = self.read_u32(offset & !3);
        (word >> ((offset & 2) * 8)) as u16
    }

    fn read_u32(&self, offset: u64) -> u32 {
        let address = self.physical_memory_offset + self.function.config_address + offset;
        unsafe { address.as_ptr::<u32>().read_volatile() }
    }

    fn write_u16(&self, offset: u64, value: u16) {
        let address = self.physical_memory_offset + self.function.config_address + offset;
        unsafe { address.as_mut_ptr::<u16>().write_volatile(value) };
    }

    fn write_u32(&self, offset: u64, value: u32) {
        let address = self.physical_memory_offset + self.function.config_address + offset;
        unsafe { address.as_mut_ptr::<u32>().write_volatile(value) };
    }

    fn bar0(&self) -> Result<u64, ReferenceError> {
        let low = self.read_u32(PCI_BAR0_OFFSET);
        if low & 1 != 0 || low & 6 == 2 || low & 6 == 6 {
            return Err(ReferenceError::PciRegion);
        }
        let mut physical = u64::from(low & !0xf);
        if low & 6 == 4 {
            physical |= u64::from(self.read_u32(PCI_BAR0_OFFSET + 4)) << 32;
        }
        if physical == 0 || physical & (PAGE_SIZE - 1) != 0 {
            return Err(ReferenceError::PciRegion);
        }
        PhysAddr::try_new(
            physical
                .checked_add(PAGE_SIZE - 1)
                .ok_or(ReferenceError::PciRegion)?,
        )
        .map_err(|_| ReferenceError::PciRegion)?;
        Ok(physical)
    }

    fn find_msi(&self) -> Result<u64, ReferenceError> {
        let mut next = self.read_u8(PCI_CAPABILITIES_OFFSET);
        let mut visited = [false; 64];
        for _ in 0..48 {
            if next == 0 {
                return Err(ReferenceError::MissingMsi);
            }
            if next < 0x40 || next & 3 != 0 {
                return Err(ReferenceError::InvalidMsi);
            }
            let index = usize::from(next / 4);
            if visited[index] {
                return Err(ReferenceError::InvalidMsi);
            }
            visited[index] = true;
            let offset = u64::from(next);
            if self.read_u8(offset) == MSI_CAPABILITY_ID {
                return Ok(offset);
            }
            next = self.read_u8(offset + 1);
        }
        Err(ReferenceError::InvalidMsi)
    }
}

struct EduMmio {
    base: VirtAddr,
    mapped: bool,
}

impl EduMmio {
    fn map<const N: usize>(
        physical: u64,
        table: &mut ActivePageTable,
        frames: &mut SystemFrameAllocator<N>,
    ) -> Result<Self, ReferenceError> {
        let base = VirtAddr::new(MMIO_VIRTUAL_ADDRESS);
        if table.translate_addr(base).is_some() {
            return Err(ReferenceError::Mapping);
        }
        let page = Page::<Size4KiB>::containing_address(base);
        let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(physical));
        let flags = PageTableFlags::WRITABLE
            | PageTableFlags::NO_EXECUTE
            | PageTableFlags::NO_CACHE
            | PageTableFlags::WRITE_THROUGH;
        unsafe { table.map_4k(page, frame, flags, frames) }.map_err(|_| ReferenceError::Mapping)?;
        Ok(Self { base, mapped: true })
    }

    fn read32(&self, offset: usize) -> u32 {
        unsafe { (self.base + offset as u64).as_ptr::<u32>().read_volatile() }
    }

    fn write32(&self, offset: usize, value: u32) {
        unsafe {
            (self.base + offset as u64)
                .as_mut_ptr::<u32>()
                .write_volatile(value)
        };
    }

    fn read64(&self, offset: usize) -> u64 {
        unsafe { (self.base + offset as u64).as_ptr::<u64>().read_volatile() }
    }

    fn write64(&self, offset: usize, value: u64) {
        unsafe {
            (self.base + offset as u64)
                .as_mut_ptr::<u64>()
                .write_volatile(value)
        };
    }

    fn unmap(&mut self, table: &mut ActivePageTable) -> Result<(), ReferenceError> {
        if !self.mapped {
            return Err(ReferenceError::Mapping);
        }
        let page = Page::<Size4KiB>::containing_address(self.base);
        table.unmap_4k(page).map_err(|_| ReferenceError::Mapping)?;
        self.mapped = false;
        Ok(())
    }
}

struct MsiConfiguration {
    offset: u64,
    control: u16,
    address_low: u32,
    address_high: u32,
    data: u16,
    is_64bit: bool,
}

impl MsiConfiguration {
    fn enable(config: &PciConfig) -> Result<Self, ReferenceError> {
        let offset = config.find_msi()?;
        let control = config.read_u16(offset + 2);
        let is_64bit = control & (1 << 7) != 0;
        let data_offset = if is_64bit { offset + 12 } else { offset + 8 };
        if data_offset + 2 > 256 {
            return Err(ReferenceError::InvalidMsi);
        }
        let address_low = config.read_u32(offset + 4);
        let address_high = if is_64bit {
            config.read_u32(offset + 8)
        } else {
            0
        };
        let data = config.read_u16(data_offset);

        config.write_u16(offset + 2, control & !1);
        config.write_u32(offset + 4, MSI_MESSAGE_ADDRESS);
        if is_64bit {
            config.write_u32(offset + 8, 0);
        }
        config.write_u16(
            data_offset,
            u16::from(crate::arch::x86_64::apic::REFERENCE_EDU_VECTOR),
        );
        config.write_u16(offset + 2, (control & !0x70) | 1);

        let saved = Self {
            offset,
            control,
            address_low,
            address_high,
            data,
            is_64bit,
        };
        if config.read_u16(offset + 2) & 1 == 0 {
            // Частично настроенный MSI нельзя оставлять действующим.
            saved.disable(config);
            return Err(ReferenceError::InvalidMsi);
        }
        Ok(saved)
    }

    fn disable(self, config: &PciConfig) {
        config.write_u16(self.offset + 2, self.control & !1);
        config.write_u32(self.offset + 4, self.address_low);
        if self.is_64bit {
            config.write_u32(self.offset + 8, self.address_high);
        }
        let data_offset = if self.is_64bit {
            self.offset + 12
        } else {
            self.offset + 8
        };
        config.write_u16(data_offset, self.data);
        config.write_u16(self.offset + 2, self.control);
    }
}

/// Только чтение подтверждённой причины прерывания, сброс причины и EOI.
/// Функция не выполняет DMA, не выделяет память и не удерживает блокировку.
pub extern "x86-interrupt" fn interrupt_handler(_frame: InterruptStackFrame) {
    let address = EDU_IRQ_ADDRESS.load(Ordering::Acquire);
    if address != 0 {
        let status = unsafe {
            (VirtAddr::new(address) + edu::MMIO_IRQ_STATUS as u64)
                .as_ptr::<u32>()
                .read_volatile()
        };
        if status != 0 {
            unsafe {
                (VirtAddr::new(address) + edu::MMIO_IRQ_ACK as u64)
                    .as_mut_ptr::<u32>()
                    .write_volatile(status);
            }
            EDU_INTERRUPTS.record(status);
        }
    }
    crate::arch::x86_64::apic::acknowledge_external_interrupt();
}

fn wait_irq(mask: u32) -> Result<(), ReferenceError> {
    for _ in 0..POLL_LIMIT {
        if EDU_INTERRUPTS.observed() & mask == mask {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(ReferenceError::InterruptTimeout)
}

struct EduDmaPlatform<'a, const N: usize> {
    frames: &'a mut SystemFrameAllocator<N>,
    physical_memory_offset: VirtAddr,
    backend: QemuDirectDma<1>,
    first_frame: Option<PhysFrame<Size4KiB>>,
}

impl<'a, const N: usize> EduDmaPlatform<'a, N> {
    fn new(frames: &'a mut SystemFrameAllocator<N>, offset: VirtAddr) -> Self {
        let aperture =
            DirectDmaAperture::new(0, edu::EDU_DMA_LIMIT).expect("допустимый диапазон прямого DMA");
        Self {
            frames,
            physical_memory_offset: offset,
            backend: unsafe { QemuDirectDma::<1>::new(aperture) },
            first_frame: None,
        }
    }
}

impl<const N: usize> DmaMappingBackend for EduDmaPlatform<'_, N> {
    fn map(
        &mut self,
        address: PhysicalAddress,
        layout: DmaLayout,
    ) -> Result<DeviceAddress, DmaError> {
        self.backend.map(address, layout)
    }

    fn unmap(&mut self, device: DeviceAddress, layout: DmaLayout) -> Result<(), DmaError> {
        self.backend.unmap(device, layout)
    }
}

impl<const N: usize> DmaBufferPlatform for EduDmaPlatform<'_, N> {
    fn reserve(&mut self, layout: DmaLayout) -> Result<PhysicalAddress, DmaError> {
        if layout.length() != PAGE_SIZE as usize
            || layout.alignment() != PAGE_SIZE as usize
            || self.first_frame.is_some()
        {
            return Err(DmaError::InvalidRange);
        }
        let frame = self
            .frames
            .allocate_contiguous_below_4k(1, 1, edu::EDU_DMA_LIMIT - 1)
            .ok_or(DmaError::Unaddressable)?;
        let address = frame.start_address().as_u64();
        self.first_frame = Some(frame);
        Ok(PhysicalAddress::new(address))
    }

    fn map_cpu(
        &mut self,
        physical: PhysicalAddress,
        _layout: DmaLayout,
    ) -> Result<CpuVirtualAddress, DmaError> {
        if self.first_frame.map(|frame| frame.start_address().as_u64()) != Some(physical.value()) {
            return Err(DmaError::InvalidRange);
        }
        let virtual_address = self
            .physical_memory_offset
            .as_u64()
            .checked_add(physical.value())
            .ok_or(DmaError::AddressOverflow)?;
        // Линейное отображение RAM уже создано загрузчиком UEFI.
        Ok(CpuVirtualAddress::new(virtual_address as usize))
    }

    fn unmap_cpu(&mut self, _cpu: CpuVirtualAddress, _layout: DmaLayout) -> Result<(), DmaError> {
        // Переиспользуем существующее линейное отображение загрузчика:
        // оно остаётся у ядра, физическую страницу возвращаем отдельно.
        Ok(())
    }

    fn free(&mut self, physical: PhysicalAddress, _: DmaLayout) -> Result<(), DmaError> {
        let frame = self.first_frame.ok_or(DmaError::InvalidRange)?;
        if frame.start_address().as_u64() != physical.value() {
            return Err(DmaError::InvalidRange);
        }
        self.frames
            .release_contiguous_4k(frame, 1)
            .map_err(|_| DmaError::InvalidRange)?;
        self.first_frame = None;
        Ok(())
    }
}

fn transfer(
    mmio: &EduMmio,
    source: u64,
    destination: u64,
    length: usize,
    command: u32,
) -> Result<(), ReferenceError> {
    mmio.write64(edu::MMIO_DMA_SRC, source);
    mmio.write64(edu::MMIO_DMA_DST, destination);
    mmio.write64(edu::MMIO_DMA_COUNT, length as u64);
    fence(Ordering::SeqCst);
    mmio.write64(edu::MMIO_DMA_COMMAND, u64::from(command));

    for _ in 0..POLL_LIMIT {
        if mmio.read64(edu::MMIO_DMA_COMMAND) & 1 == 0 {
            fence(Ordering::SeqCst);
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(ReferenceError::DmaTimeout)
}

fn exercise_dma<const N: usize>(
    mmio: &EduMmio,
    frames: &mut SystemFrameAllocator<N>,
    physical_memory_offset: VirtAddr,
) -> Result<(usize, u64), ReferenceError> {
    let initial_free = frames.free_frames();
    let mut platform = EduDmaPlatform::new(frames, physical_memory_offset);
    let layout = DmaLayout::new(PAGE_SIZE as usize, PAGE_SIZE as usize)
        .map_err(|_| ReferenceError::DmaAddress)?;
    let mut dma = DmaBuffer::reserve(&mut platform, layout).map_err(ReferenceError::Dma)?;
    let ready = (|| -> Result<_, ReferenceError> {
        dma.map_cpu(&mut platform).map_err(ReferenceError::Dma)?;
        dma.map_device(&mut platform).map_err(ReferenceError::Dma)?;
        let region = dma.region().map_err(ReferenceError::Dma)?;
        edu::check_dma_range(region.device_address().value(), edu::EDU_TRANSFER_BYTES)
            .map_err(|_| ReferenceError::DmaAddress)?;
        Ok(region)
    })();
    let region = match ready {
        Ok(region) => region,
        Err(error) => {
            dma.close(&mut platform).map_err(ReferenceError::Dma)?;
            return Err(error);
        }
    };
    let pointer = region.cpu_address().value() as *mut u8;
    for index in 0..edu::EDU_TRANSFER_BYTES {
        unsafe {
            pointer
                .add(index)
                .write_volatile((index as u8).wrapping_mul(7).wrapping_add(11))
        };
    }
    fence(Ordering::SeqCst);

    if let Err(error) = dma.begin_device_access() {
        dma.close(&mut platform).map_err(ReferenceError::Dma)?;
        return Err(ReferenceError::Dma(error));
    }
    transfer(
        mmio,
        region.device_address().value(),
        edu::EDU_INTERNAL_BUFFER,
        edu::EDU_TRANSFER_BYTES,
        edu::EDU_DMA_START,
    )?;

    for index in 0..edu::EDU_TRANSFER_BYTES {
        unsafe { pointer.add(index).write_volatile(0) };
    }
    fence(Ordering::SeqCst);
    transfer(
        mmio,
        edu::EDU_INTERNAL_BUFFER,
        region.device_address().value(),
        edu::EDU_TRANSFER_BYTES,
        edu::EDU_DMA_START | edu::EDU_DMA_FROM_DEVICE | edu::EDU_DMA_INTERRUPT,
    )?;
    // Оба аппаратных переноса завершились. Даже если не придёт ожидаемый
    // MSI или проверка содержимого обнаружит ошибку, страницу необходимо
    // вернуть после подтверждённой остановки устройства.
    dma.finish_device_access().map_err(ReferenceError::Dma)?;
    let verification = (|| -> Result<(), ReferenceError> {
        wait_irq(edu::EDU_IRQ_DMA)?;
        for index in 0..edu::EDU_TRANSFER_BYTES {
            let expected = (index as u8).wrapping_mul(7).wrapping_add(11);
            if unsafe { pointer.add(index).read_volatile() } != expected {
                return Err(ReferenceError::UnexpectedData);
            }
        }
        Ok(())
    })();
    dma.close(&mut platform).map_err(ReferenceError::Dma)?;
    if platform.frames.free_frames() != initial_free {
        return Err(ReferenceError::DmaNotReleased);
    }
    verification?;
    Ok((edu::EDU_TRANSFER_BYTES, 1))
}

/// На выбранном QEMU EDU реально обменивается данными и принимает MSI.
/// При отказе DMA-страницы не возвращаются до подтверждённой остановки
/// контроллера. Это не интерфейс для физической PCI-платы.
///
/// # Безопасность
///
/// Только виртуальная машина QEMU, 1 процессор, прямое DMA без IOMMU,
/// выделенный вектор MSI и закреплённый 28-битный физический диапазон.
pub unsafe fn self_test<const N: usize>(
    function: PcieFunction,
    table: &mut ActivePageTable,
    frames: &mut SystemFrameAllocator<N>,
    physical_memory_offset: VirtAddr,
) -> Result<ReferenceReport, ReferenceError> {
    let mut context = ProbeContext::default();
    let launch =
        edu::start_reference(function, &mut context).map_err(|_| ReferenceError::Registry)?;
    if context.discovered != Some(function) {
        return Err(ReferenceError::UnsupportedDevice);
    }
    let config = PciConfig {
        physical_memory_offset,
        function,
    };
    let physical = config.bar0()?;
    let mut mmio = EduMmio::map(physical, table, frames)?;
    let validation = (|| -> Result<(), ReferenceError> {
        let identifier = mmio.read32(edu::MMIO_IDENTIFICATION);
        if identifier & 0xff != 0xed {
            return Err(ReferenceError::InvalidIdentity);
        }
        mmio.write32(edu::MMIO_LIVENESS, 0x1357_2468);
        if mmio.read32(edu::MMIO_LIVENESS) != !0x1357_2468_u32 {
            return Err(ReferenceError::InvalidIdentity);
        }
        Ok(())
    })();
    if let Err(error) = validation {
        mmio.unmap(table)?;
        return Err(error);
    }

    let initial_command = config.read_u16(PCI_COMMAND_OFFSET);
    config.write_u16(
        PCI_COMMAND_OFFSET,
        initial_command | PCI_MEMORY_ENABLE | PCI_BUS_MASTER | PCI_INTX_DISABLE,
    );
    let msi = x86_64::instructions::interrupts::without_interrupts(|| {
        EDU_INTERRUPTS.reset();
        EDU_IRQ_ADDRESS.store(mmio.base.as_u64(), Ordering::Release);
        MsiConfiguration::enable(&config)
    });
    let msi = match msi {
        Ok(msi) => msi,
        Err(error) => {
            EDU_IRQ_ADDRESS.store(0, Ordering::Release);
            config.write_u16(PCI_COMMAND_OFFSET, initial_command & !PCI_BUS_MASTER);
            mmio.unmap(table)?;
            return Err(error);
        }
    };

    let result = (|| -> Result<(usize, u64, u32), ReferenceError> {
        mmio.write32(edu::MMIO_IRQ_RAISE, edu::EDU_IRQ_PROBE);
        wait_irq(edu::EDU_IRQ_PROBE)?;
        let (bytes, returned) = exercise_dma(&mmio, frames, physical_memory_offset)?;
        let mask = EDU_INTERRUPTS.observed();
        if mask & (edu::EDU_IRQ_PROBE | edu::EDU_IRQ_DMA)
            != (edu::EDU_IRQ_PROBE | edu::EDU_IRQ_DMA)
        {
            return Err(ReferenceError::InterruptTimeout);
        }
        Ok((bytes, returned, mask))
    })();

    // Снимаем MSI и адрес обработчика на любом выходе, включая таймауты
    // и ошибки данных. При незавершённом DMA его страницу не возвращаем:
    // это безопаснее, чем повторно выделить её другому владельцу.
    x86_64::instructions::interrupts::without_interrupts(|| {
        msi.disable(&config);
        EDU_IRQ_ADDRESS.store(0, Ordering::Release);
    });
    let restored = if result.is_ok() {
        initial_command
    } else {
        initial_command & !PCI_BUS_MASTER
    };
    config.write_u16(PCI_COMMAND_OFFSET, restored);
    mmio.unmap(table)?;
    let (verified_bytes, returned_dma_pages, irq_mask) = result?;

    Ok(ReferenceReport {
        pci_bus: function.bus,
        pci_device: function.device,
        verified_bytes,
        irq_mask,
        returned_dma_pages,
        driver_name: launch.name,
    })
}

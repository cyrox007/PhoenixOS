//! Системная очередь ввода: один защищённый поток событий для процессов.
//!
//! В проверочном QEMU-профиле xHCI публикует события из основного потока
//! ядра. Доступ к очереди через системный вызов разрешён только процессу
//! с возможностью чтения выбранного устройства. Доставка не использует
//! кучу и не создаёт второй путь приёма USB.

use phoenix_input::InputQueue;
use phoenix_pcie::usb_hid::InputEvent;
use phoenix_syscall_abi::InputEventRecord;
use spin::Mutex;

pub const INPUT_CAPABILITY_OBJECT: u64 = 0x5048_5849_4e50_5554;
pub const INPUT_QUEUE_CAPACITY: usize = 64;

static EVENTS: Mutex<InputQueue<INPUT_QUEUE_CAPACITY>> = Mutex::new(InputQueue::new());

pub fn with_events<T>(operation: impl FnOnce(&mut InputQueue<INPUT_QUEUE_CAPACITY>) -> T) -> T {
    let mut queue = EVENTS.lock();
    operation(&mut queue)
}

pub fn publish_hid(source: u8, event: InputEvent) {
    let record = match event {
        InputEvent::Key(key) => InputEventRecord::key(
            u32::from(source),
            u32::from(key.usage),
            key.pressed,
            u32::from(key.modifiers),
        ),
        InputEvent::Mouse(mouse) => InputEventRecord::mouse(
            u32::from(source),
            u32::from(mouse.buttons),
            i32::from(mouse.delta_x),
            i32::from(mouse.delta_y),
            i32::from(mouse.wheel),
        ),
    };
    publish(record);
}

pub fn publish_device(source: u8, connected: bool) {
    publish(InputEventRecord::device(u32::from(source), connected));
}

fn publish(event: InputEventRecord) {
    // Ненулевая константа ёмкости делает невозможной ошибку размещения.
    with_events(|events| {
        events
            .push(event)
            .expect("системная очередь ввода должна иметь ненулевую вместимость");
    });
}

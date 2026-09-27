#![no_std]

use phoenix_capability::{CapabilityError, CapabilityHandle};
use phoenix_process::{ProcessCapabilitySet, ProcessId};

pub const MAX_MESSAGE_WORDS: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcError {
    QueueFull,
    QueueEmpty,
    TooManyWords,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcSendError {
    Transport(IpcError),
    Capability(CapabilityError),
}

impl From<IpcError> for IpcSendError {
    fn from(error: IpcError) -> Self {
        Self::Transport(error)
    }
}

impl From<CapabilityError> for IpcSendError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message {
    pub sender: ProcessId,
    words: [u64; MAX_MESSAGE_WORDS],
    len: u8,
    transferred_capability: Option<CapabilityHandle>,
}

impl Message {
    pub fn new(sender: ProcessId, words: &[u64]) -> Result<Self, IpcError> {
        if words.len() > MAX_MESSAGE_WORDS {
            return Err(IpcError::TooManyWords);
        }

        let mut message = Self {
            sender,
            words: [0; MAX_MESSAGE_WORDS],
            len: words.len() as u8,
            transferred_capability: None,
        };

        let mut index = 0;
        while index < words.len() {
            message.words[index] = words[index];
            index += 1;
        }

        Ok(message)
    }

    pub const fn len(&self) -> usize {
        self.len as usize
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn words(&self) -> &[u64] {
        &self.words[..self.len()]
    }

    pub const fn transferred_capability(&self) -> Option<CapabilityHandle> {
        self.transferred_capability
    }

    fn set_transferred_capability(&mut self, handle: CapabilityHandle) {
        self.transferred_capability = Some(handle);
    }
}

pub struct Endpoint<const CAPACITY: usize> {
    id: EndpointId,
    queue: [Option<Message>; CAPACITY],
    head: usize,
    tail: usize,
    len: usize,
}

impl<const CAPACITY: usize> Endpoint<CAPACITY> {
    pub const fn new(id: EndpointId) -> Self {
        Self {
            id,
            queue: [None; CAPACITY],
            head: 0,
            tail: 0,
            len: 0,
        }
    }

    pub const fn id(&self) -> EndpointId {
        self.id
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub const fn is_full(&self) -> bool {
        self.len == CAPACITY
    }

    pub fn send(&mut self, message: Message) -> Result<(), IpcError> {
        if CAPACITY == 0 || self.is_full() {
            return Err(IpcError::QueueFull);
        }

        self.push(message);
        Ok(())
    }

    pub fn receive(&mut self) -> Result<Message, IpcError> {
        if self.is_empty() {
            return Err(IpcError::QueueEmpty);
        }

        let message = self.queue[self.head].take().ok_or(IpcError::QueueEmpty)?;
        self.head = (self.head + 1) % CAPACITY;
        self.len -= 1;

        Ok(message)
    }

    fn push(&mut self, message: Message) {
        self.queue[self.tail] = Some(message);
        self.tail = (self.tail + 1) % CAPACITY;
        self.len += 1;
    }
}

pub fn send_with_capability<
    const ENDPOINT_CAPACITY: usize,
    const SOURCE_CAPACITY: usize,
    const TARGET_CAPACITY: usize,
>(
    endpoint: &mut Endpoint<ENDPOINT_CAPACITY>,
    source: &mut ProcessCapabilitySet<SOURCE_CAPACITY>,
    target: &mut ProcessCapabilitySet<TARGET_CAPACITY>,
    words: &[u64],
    handle: CapabilityHandle,
) -> Result<(), IpcSendError> {
    if ENDPOINT_CAPACITY == 0 || endpoint.is_full() {
        return Err(IpcError::QueueFull.into());
    }

    let mut message = Message::new(source.owner(), words)?;
    let received_handle = source.transfer_to(target, handle)?;
    message.set_transferred_capability(received_handle);
    endpoint.push(message);

    Ok(())
}

impl<const CAPACITY: usize> Default for Endpoint<CAPACITY> {
    fn default() -> Self {
        Self::new(EndpointId(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_preserves_fifo_order_and_sender() {
        let mut endpoint = Endpoint::<2>::new(EndpointId(7));
        let first = Message::new(ProcessId(10), &[1, 2]).unwrap();
        let second = Message::new(ProcessId(20), &[3]).unwrap();

        endpoint.send(first).unwrap();
        endpoint.send(second).unwrap();

        assert_eq!(endpoint.receive().unwrap(), first);
        assert_eq!(endpoint.receive().unwrap(), second);
        assert_eq!(endpoint.receive(), Err(IpcError::QueueEmpty));
    }

    #[test]
    fn endpoint_rejects_send_when_full() {
        let mut endpoint = Endpoint::<1>::new(EndpointId(1));
        endpoint
            .send(Message::new(ProcessId(1), &[11]).unwrap())
            .unwrap();

        assert_eq!(
            endpoint.send(Message::new(ProcessId(2), &[22]).unwrap()),
            Err(IpcError::QueueFull)
        );
    }

    #[test]
    fn message_rejects_payload_larger_than_inline_capacity() {
        let words = [1, 2, 3, 4, 5, 6, 7];

        assert_eq!(
            Message::new(ProcessId(1), &words),
            Err(IpcError::TooManyWords)
        );
    }

    #[test]
    fn queue_wraparound_keeps_fifo_order() {
        let mut endpoint = Endpoint::<2>::new(EndpointId(9));
        let first = Message::new(ProcessId(1), &[10]).unwrap();
        let second = Message::new(ProcessId(2), &[20]).unwrap();
        let third = Message::new(ProcessId(3), &[30]).unwrap();

        endpoint.send(first).unwrap();
        endpoint.send(second).unwrap();
        assert_eq!(endpoint.receive().unwrap(), first);

        endpoint.send(third).unwrap();

        assert_eq!(endpoint.receive().unwrap(), second);
        assert_eq!(endpoint.receive().unwrap(), third);
        assert!(endpoint.is_empty());
    }

    #[test]
    fn zero_capacity_endpoint_is_always_full() {
        let mut endpoint = Endpoint::<0>::new(EndpointId(0));
        let message = Message::new(ProcessId(1), &[]).unwrap();

        assert!(endpoint.is_full());
        assert_eq!(endpoint.send(message), Err(IpcError::QueueFull));
        assert_eq!(endpoint.receive(), Err(IpcError::QueueEmpty));
    }

    fn transferable_capability(object: u64) -> phoenix_capability::Capability {
        phoenix_capability::Capability {
            object: phoenix_capability::ObjectId(object),
            kind: phoenix_capability::ObjectKind::Endpoint,
            rights: phoenix_capability::Rights::READ.union(phoenix_capability::Rights::TRANSFER),
        }
    }

    #[test]
    fn capability_transfer_is_delivered_with_message() {
        let mut endpoint = Endpoint::<1>::new(EndpointId(5));
        let mut source = ProcessCapabilitySet::<1>::new(ProcessId(10));
        let mut target = ProcessCapabilitySet::<1>::new(ProcessId(20));
        let source_handle = source.insert(transferable_capability(77)).unwrap();

        send_with_capability(
            &mut endpoint,
            &mut source,
            &mut target,
            &[1, 2],
            source_handle,
        )
        .unwrap();

        let message = endpoint.receive().unwrap();
        let received_handle = message.transferred_capability().unwrap();

        assert_eq!(message.sender, ProcessId(10));
        assert_eq!(message.words(), &[1, 2]);
        assert_eq!(
            source.get(source_handle),
            Err(CapabilityError::InvalidHandle)
        );
        assert_eq!(target.get(received_handle).unwrap().object.0, 77);
    }

    #[test]
    fn full_endpoint_does_not_move_capability() {
        let mut endpoint = Endpoint::<1>::new(EndpointId(5));
        let mut source = ProcessCapabilitySet::<1>::new(ProcessId(10));
        let mut target = ProcessCapabilitySet::<1>::new(ProcessId(20));
        let source_handle = source.insert(transferable_capability(88)).unwrap();

        endpoint
            .send(Message::new(ProcessId(1), &[9]).unwrap())
            .unwrap();

        assert_eq!(
            send_with_capability(&mut endpoint, &mut source, &mut target, &[1], source_handle,),
            Err(IpcSendError::Transport(IpcError::QueueFull))
        );
        assert_eq!(source.get(source_handle).unwrap().object.0, 88);
        assert!(target.is_empty());
    }

    #[test]
    fn full_target_does_not_enqueue_or_move_capability() {
        let mut endpoint = Endpoint::<1>::new(EndpointId(5));
        let mut source = ProcessCapabilitySet::<1>::new(ProcessId(10));
        let mut target = ProcessCapabilitySet::<1>::new(ProcessId(20));
        let source_handle = source.insert(transferable_capability(99)).unwrap();
        target.insert(transferable_capability(100)).unwrap();

        assert_eq!(
            send_with_capability(&mut endpoint, &mut source, &mut target, &[1], source_handle,),
            Err(IpcSendError::Capability(CapabilityError::CapacityExceeded))
        );
        assert_eq!(source.get(source_handle).unwrap().object.0, 99);
        assert!(endpoint.is_empty());
    }
}

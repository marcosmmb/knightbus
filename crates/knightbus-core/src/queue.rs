use crate::{Budget, Error, Result, TransferEvent, resource::Credit};

// Each lease holds only two indices. All completion storage is prepaid globally;
// enqueue, dequeue and reclamation never allocate or move unrelated completions.
#[derive(Default)]
pub(crate) struct ReplyQueue {
    head: Option<usize>,
    tail: Option<usize>,
}
struct Slot {
    event: Option<TransferEvent>,
    next: Option<usize>,
}
pub(crate) struct ReplyPool {
    slots: Vec<Slot>,
    free: Option<usize>,
    used: usize,
    _credit: Credit,
}
impl ReplyPool {
    pub(crate) fn new(budget: &Budget, count: usize) -> Result<Self> {
        let size = std::mem::size_of::<Slot>();
        let mut credit = budget.reserve(count.checked_mul(size).ok_or(Error::Exhausted)?, 0)?;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(count)
            .map_err(|_| Error::Exhausted)?;
        credit.grow((slots.capacity() - count) * size)?;
        slots.extend((0..count).map(|i| Slot {
            event: None,
            next: (i + 1 < count).then_some(i + 1),
        }));
        Ok(Self {
            slots,
            free: (count > 0).then_some(0),
            used: 0,
            _credit: credit,
        })
    }
    pub(crate) fn len(&self) -> usize {
        self.used
    }
    pub(crate) fn push(&mut self, queue: &mut ReplyQueue, event: TransferEvent) {
        let index = self.free.expect("submission reserved a completion slot");
        self.free = self.slots[index].next;
        self.slots[index] = Slot {
            event: Some(event),
            next: None,
        };
        if let Some(tail) = queue.tail {
            self.slots[tail].next = Some(index);
        } else {
            queue.head = Some(index);
        }
        queue.tail = Some(index);
        self.used += 1;
    }
    pub(crate) fn pop(&mut self, queue: &mut ReplyQueue) -> Option<TransferEvent> {
        let index = queue.head?;
        let slot = &mut self.slots[index];
        queue.head = slot.next;
        if queue.head.is_none() {
            queue.tail = None;
        }
        slot.next = self.free;
        self.free = Some(index);
        self.used -= 1;
        slot.event.take()
    }
}

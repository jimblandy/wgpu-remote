/*! A [`Sender`][t] implementation where both sides live in the same process.

This module's [`LocalSender`] type is an implementation of the
[`Sender`][t] trait where both counterparts actually live in the same
process. It's not "remote" at all. This is mostly intended for
testing.

[t]: crate::transport::Sender

*/

use crate::{transport as tp, Block};
use hashbrown::HashMap;
use std::ops::Range;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// A [`transport::Sender`][t] where both counterparts are in the same process.
///
/// A [`LocalSender`] transport connects a wgpu client and server running in
/// the same address space. "Shared memory" blocks are ordinary
/// regions of memory. A new thread is spawned to make calls to
/// the receiver.
///
/// [t]: crate::transport::Sender
pub struct LocalSender {
    next_shared_memory_id: usize,
    shmems: Arc<Mutex<ShmemTable>>,
    counterpart_shmems: Arc<Mutex<ShmemTable>>,
    event_queue: mpsc::SyncSender<Message>,
}

type ShmemTable = HashMap<tp::SharedMemoryHandle, Arc<Block>>;

impl LocalSender {
    /// Create a pair of `Local` senders.
    ///
    /// Return two entangled [`LocalSender`] senders that can exchange
    /// messages with each other.
    ///
    /// This function returns two [`TransportSide`] values, `(left, right)`, such that:
    ///
    /// - A message sent to `left.sender` will be received by the
    ///   [`DynReceiver`] passed to `right.register_callback`.
    ///
    /// - A message sent to `right.sender` will be received by the
    ///   [`DynReceiver`] passed to `left.register_callback`.
    ///
    /// Dropping a `Sender` causes the `DynReceiver` that delivers its
    /// messages to be dropped as well.
    ///
    /// [`DynReceiver`]: crate::transport::DynReceiver
    pub fn new_pair() -> (tp::TransportSide<Self>, tp::TransportSide<Self>) {
        let left_shmem_table = Default::default();
        let right_shmem_table = Default::default();

        let left_to_right = mpsc::sync_channel(0);
        let right_to_left = mpsc::sync_channel(0);

        let left_sender = Self {
            next_shared_memory_id: 0,
            shmems: Arc::clone(&left_shmem_table),
            counterpart_shmems: Arc::clone(&right_shmem_table),
            event_queue: left_to_right.0,
        };

        let right_sender = Self {
            next_shared_memory_id: 1,
            shmems: Arc::clone(&right_shmem_table),
            counterpart_shmems: Arc::clone(&left_shmem_table),
            event_queue: right_to_left.0,
        };

        let left = tp::TransportSide {
            sender: left_sender,
            register_callback: Box::new(move |receiver| start_receiver_thread(receiver, right_to_left.1, "LocalSender::left")),
        };
        let right = tp::TransportSide {
            sender: right_sender,
            register_callback: Box::new(move |receiver| start_receiver_thread(receiver, left_to_right.1, "LocalSender::right")),
        };

        (left, right)
    }
}

impl tp::Sender for LocalSender {
    fn allocate_shared_memory(&mut self, size: usize) -> std::io::Result<tp::SharedMemoryHandle> {
        let shared_block = Arc::new(make_global_block(size)?);

        let handle = tp::SharedMemoryHandle(self.next_shared_memory_id);
        self.next_shared_memory_id += 2;

        self.counterpart_shmems
            .lock()
            .unwrap()
            .insert(handle, Arc::clone(&shared_block));
        self.shmems.lock().unwrap().insert(handle, shared_block);

        Ok(handle)
    }

    fn close_shared_memory(&mut self, handle: tp::SharedMemoryHandle) {
        assert!(self.shmems.lock().unwrap().remove(&handle).is_some());
    }

    fn map_shared_memory(&mut self, handle: tp::SharedMemoryHandle) -> Block {
        let guard = self.shmems.lock().unwrap();
        match guard.get(&handle) {
            Some(block) => make_alias_block(Arc::clone(block)),
            None => panic!("wgpu_remote::Local: no shared memory with handle {handle:?}"),
        }
    }

    fn send_message(
        &mut self,
        handle: tp::SharedMemoryHandle,
        range: Range<usize>,
    ) -> std::io::Result<()> {
        match self.event_queue.send(Message { handle, range }) {
            Ok(()) => Ok(()),
            Err(_) => Err(std::io::Error::other(
                "wgpu_remote::Local: counterpart has exited",
            )),
        }
    }

    fn flush_shared_memory_range(
        &mut self,
        handle: tp::SharedMemoryHandle,
        range: Range<usize>,
    ) -> std::io::Result<()> {
        let _ = (handle, range);
        Ok(())
    }
}

/// Spawn a thread to deliver messages.
///
/// Start a thread that delivers messages that arrive on `incoming` to
/// `receiver`. Use `name` as the thread's name.
fn start_receiver_thread(mut receiver: Box<tp::DynReceiver>, incoming: mpsc::Receiver<Message>, name: &'static str) {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            for Message {
                handle: shmem,
                range,
            } in incoming
            {
                if receiver.receive_message(shmem, range).is_err() {
                    break;
                }
            }
            eprintln!(
                "ReceiverStarter::start: thread `{name}` exiting",
            );
        })
        .expect("failed to start `LocalSender` receiver thread for {name}");
}

struct Message {
    handle: tp::SharedMemoryHandle,
    range: Range<usize>,
}

/// Create a block from the global allocator.
///
/// Return a block of at least `size` bytes, aligned to [`SHARED_MEMORY_ALIGNMENT`],
/// allocated from the `std` global allocator.
///
/// [`SHARED_MEMORY_ALIGNMENT`]: tp::SHARED_MEMORY_ALIGNMENT
fn make_global_block(size: usize) -> std::io::Result<Block> {
    let size = std::cmp::max(1, size).next_multiple_of(tp::SHARED_MEMORY_ALIGNMENT);
    let layout = std::alloc::Layout::from_size_align(size, tp::SHARED_MEMORY_ALIGNMENT).unwrap();
    let bytes: *mut [u8] = unsafe {
        // Safety: we ensured that size is non-zero above.
        let ptr = std::alloc::alloc_zeroed(layout);
        if ptr.is_null() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::OutOfMemory,
                "wgpu_remote::Local failed to allocate shared memory",
            ));
        }

        // Safety: `ptr` points to initialized bytes of the required size.
        std::slice::from_raw_parts_mut::<u8>(ptr, size)
    };

    let free = move |bytes: *mut [u8]| unsafe {
        std::alloc::dealloc(bytes as *mut u8, layout);
    };

    // Safety:
    // - `bytes` is a valid pointer for reading and writing, and
    //   will live until `free` is called.
    // - There are no other pointers to `bytes`.
    let block = unsafe { Block::new(bytes, free) };

    Ok(block)
}

/// Create a block that hands out aliases to `base`.
///
/// Create a block that holds a strong reference to `base`, and
/// returns the same memory it does.
fn make_alias_block(base: Arc<Block>) -> Block {
    let bytes = base.bytes_mut();
    let free = move |_bytes| drop(base);
    // Safety: `free` has taken ownership of `base`, so `bytes` will remain
    // valid until `free` is called.
    unsafe { Block::new(bytes, free) }
}

#[cfg(test)]
mod tests {
    // TODO: just use Local
    use super::*;
    use std::sync::mpsc;
    use tp::Sender as _;

    type MpscReceiver = mpsc::Receiver<Message>;

    struct MockReceiver {
        tx: mpsc::Sender<Message>,
    }

    impl MockReceiver {
        fn new() -> (Self, MpscReceiver) {
            let (tx, rx) = mpsc::channel();
            let mock = Self { tx };
            (mock, rx)
        }
    }

    impl tp::Receiver for MockReceiver {
        fn receive_message(
            &mut self,
            handle: tp::SharedMemoryHandle,
            range: Range<usize>,
        ) -> Result<(), tp::ReceiverError> {
            self.tx.send(Message { handle, range }).unwrap();
            Ok(())
        }
    }

    fn mock_pair() -> ((LocalSender, LocalSender), (MpscReceiver, MpscReceiver)) {
        let (left_rx, left_mpsc_rx) = MockReceiver::new();
        let (right_rx, right_mpsc_rx) = MockReceiver::new();

        let (left, right) = LocalSender::new_pair();
        (left.register_callback)(Box::new(left_rx));
        (right.register_callback)(Box::new(right_rx));
        ((left.sender, right.sender), (left_mpsc_rx, right_mpsc_rx))
    }

    #[test]
    fn shared_memory_allocation_and_mapping() {
        let ((mut tx, _), (_, _)) = mock_pair();

        let handle = tx.allocate_shared_memory(128).expect("Allocation failed");
        let block = tx.map_shared_memory(handle);
        let bytes = block.bytes_mut();
        assert_eq!(
            bytes.len(),
            128_usize.next_multiple_of(tp::SHARED_MEMORY_ALIGNMENT)
        );
        tx.close_shared_memory(handle);
    }

    #[test]
    fn send_and_receive() {
        let ((mut left, _right), (_left_rx, right_rx)) = mock_pair();
        let handle = left.allocate_shared_memory(64).unwrap();
        left.send_message(handle, 10..20).unwrap();

        // Dropping a `tp::Sender` should drop the corresponding `tp::Receiver`.
        // Dropping a `MockReceiver` should close its `mpsc::Sender`,
        // causing iteration over `right_rx` to terminate.
        drop(left);

        // Iterating over an `mpsc::Receiver` ends when the
        // corresponding `mpsc::Sender` is closed.
        let messages: Vec<_> = right_rx.into_iter().collect();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].handle, handle);
        assert_eq!(messages[0].range, 10..20);
    }

    #[test]
    #[should_panic(expected = "no shared memory with handle")]
    fn map_invalid_handle_panics() {
        let ((mut left, _), (_, _)) = mock_pair();
        left.map_shared_memory(tp::SharedMemoryHandle(999));
    }

    #[test]
    fn shared_memory_owned_by_both_sides() {
        let ((mut left, mut right), (_, _)) = mock_pair();

        let left_handle = left
            .allocate_shared_memory(128)
            .expect("failed to allocate shared memory");

        // Both left and right can map this.
        let _ = left.map_shared_memory(left_handle);
        let _ = right.map_shared_memory(left_handle);

        // If right closes, it, left can still map it.
        right.close_shared_memory(left_handle);
        let _ = left.map_shared_memory(left_handle);

        // But the right can't close it twice.
        // (This drops `right`, because I'm not sure `Local` is `UnwindSafe`.)
        let result = std::panic::catch_unwind(move || right.close_shared_memory(left_handle));
        assert!(result.is_err());

        // left can still map it, even now.
        let _ = left.map_shared_memory(left_handle);

        left.close_shared_memory(left_handle);
        let result = std::panic::catch_unwind(move || left.close_shared_memory(left_handle));
        assert!(result.is_err());
    }

    #[test]
    fn conversation() {
        struct GastonRx(Option<LocalSender>, mpsc::Sender<&'static str>);
        impl tp::Receiver for GastonRx {
            fn receive_message(
                &mut self,
                handle: tp::SharedMemoryHandle,
                range: Range<usize>,
            ) -> Result<(), tp::ReceiverError> {
                let mut sender = self.0.take().unwrap();
                {
                    let block = sender.map_shared_memory(handle);
                    // Safety: We're holding `block`, and Alphonse isn't going
                    // to write to its contents.
                    let slice = unsafe { &*block.bytes_mut() };
                    assert_eq!(&slice[range], b"Good morning, my dear Gaston!");
                }

                {
                    let response_shmem = sender
                        .allocate_shared_memory(128)
                        .expect("Gaston failed to allocate shmem");
                    let block = sender.map_shared_memory(response_shmem);
                    // Safety: We're holding `block`, and we're the
                    // only one who knows about the shmem.
                    let slice = unsafe { &mut *block.bytes_mut() };
                    slice[..36].copy_from_slice(b"But it is evening, my dear Alphonse!");
                    self.1.send("Gaston replies").unwrap();
                    sender
                        .send_message(response_shmem, 0..36)
                        .expect("Gaston failed to send response");
                }

                drop(sender);

                Ok(())
            }
        }

        struct AlphonseRx(Option<LocalSender>, mpsc::Sender<&'static str>);
        impl tp::Receiver for AlphonseRx {
            fn receive_message(
                &mut self,
                handle: tp::SharedMemoryHandle,
                range: Range<usize>,
            ) -> Result<(), tp::ReceiverError> {
                let mut sender = self.0.take().unwrap();
                let block = sender.map_shared_memory(handle);
                // Safety: We're holding `block`, and Gaston isn't going
                // to write to its contents.
                let slice = unsafe { &*block.bytes_mut() };
                assert_eq!(&slice[range], b"But it is evening, my dear Alphonse!");
                self.1.send("Alphonse got reply").unwrap();
                drop(sender);
                Ok(())
            }
        }

        let (log_tx, log_rx) = mpsc::channel();
        let (mut alphonse, gaston) = LocalSender::new_pair();

        (gaston.register_callback)(
            Box::new(GastonRx(Some(gaston.sender), log_tx.clone())),
        );

        {
            let shmem = alphonse
                .sender
                .allocate_shared_memory(128)
                .expect("Alphonse failed to allocate shmem");
            let block = alphonse.sender.map_shared_memory(shmem);

            // Safety: We're holding `block`, and we're the only one
            // who knows about the shmem.
            let slice = unsafe { &mut *block.bytes_mut() };
            slice[..29].copy_from_slice(b"Good morning, my dear Gaston!");
            log_tx.send("Alphonse greets Gaston").unwrap();
            alphonse
                .sender
                .send_message(shmem, 0..29)
                .expect("Alphonse send failed");
        }

        (alphonse.register_callback)(
            Box::new(AlphonseRx(Some(alphonse.sender), log_tx.clone())),
        );
        drop(log_tx);

        assert_eq!(log_rx.recv(), Ok("Alphonse greets Gaston"));
        assert_eq!(log_rx.recv(), Ok("Gaston replies"));
        assert_eq!(log_rx.recv(), Ok("Alphonse got reply"));
        assert!(log_rx.recv().is_err());
    }
}

/*! Definition of the `BufferedChannel` type. */

use crate::Block;
use crate::transport;

use std::marker::PhantomData;
use std::ops::Range;
use std::sync::Arc;

/// A channel carrying values of type `T` over a [`transport`].
///
/// 
pub struct BufferedChannel<T> {
    sender: Arc<transport::DynSender>,

    /// The shared memory segment holding the ring buffer of messages.
    ///
    /// Its size in bytes is always a multiple of
    /// `std::mem::align_of::<T>`.
    ring_buffer: Block,

    /// The range of values that have been serialized into `ring_buffer`,
    /// but not yet sent.
    ///
    /// Even though `ring_buffer` is a ring buffer, this is never 
    ///
    /// The start and end of this range are always a multiple of
    /// `std::mem::align_of::<T>()`.
    unsent: Range<usize>,
    
    /// The range of values that have been sent, but not yet acknowledged
    
    /// If this is equal to `buffer.len()`, then the buffer is full.
    /// If this is zero, 
    next_free: usize,

    /// Buffered channels accept, hold, and produce values of type
    /// `T`. Well, their serialized forms, but that's a detail.
    _marker: PhantomData<fn(T) -> T>,
}

impl<T> BufferedChannel<T> {
    fn new(transport: transport::Side<Arc<transport::DynSender>>, ) -> transport::Side<Self> {
        todo!()
    }
}

pub trait Receiver<T> {
    fn receive_message(
        &mut self,
        value: T
    ) -> Result<(), crate::transport::ReceiverError>;

    /// Report an error encountered trying to receive a message.
    ///
    /// After this call returns, this `Receiver` will be dropped.
    fn receive_error(&mut self, error: std::io::Error) {
        let _ = error;
    }
}

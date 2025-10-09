//! Definition of the [`Block`] type.

use std::mem::ManuallyDrop;

/// A block of bytes, with a callback to free it.
///
/// A `Block` represents a slice of bytes managed by an external allocator. The
/// `Block` also keeps a callback that will return the bytes to that external
/// allocator when the `Block` is dropped.
///
/// This crate uses `Block`s to track regions of shared memory obtained from a
/// [`Sender`] implementation, subdividing that into smaller allocations for
/// individual `wgpu` buffers using [`Pool`].
///
/// [`Sender`]: crate::transport::Sender
/// [`Pool`]: crate::Pool
//
// If `allocator_api` were stabilized, then this could just be `Box<[u8]>`.
pub struct Block {
    /// A block of bytes, aligned on an eight-byte boundary.
    // TODO: use NonNull?
    bytes: *mut [u8],

    /// The function to which `bytes` should be passed when this `Block` is dropped.
    // TODO: make the allocator a type parameter to `Block`.
    free: ManuallyDrop<Box<dyn FnOnce(*mut [u8]) + Send + 'static>>,
}

/// Safety: Raw pointers like `Block::bytes` are not `Send` by
/// default, but we only expose `bytes` when given a `&mut self`, so
/// it should be fine.
unsafe impl Send for Block {}
unsafe impl Sync for Block {}

impl Block {
    /// Create a [`Block`] tracking `bytes`.
    ///
    /// When the returned `Block` is dropped, it will call `free`,
    /// passing it the pointer to the block's bytes.
    ///
    /// # Safety
    ///
    /// The given `bytes` pointer must be valid for reading and writing the
    /// entire slice. It must remain so until it is passed to `free` by the new
    /// `Block`'s [`Drop`] implementation.
    ///
    /// The given `bytes` must not be accessed through any other pointer until
    /// after the new `Block` is dropped.
    pub unsafe fn new(bytes: *mut [u8], free: impl FnOnce(*mut [u8]) + Send + 'static) -> Block {
        Self {
            bytes,
            free: ManuallyDrop::new(Box::new(free)),
        }
    }

    /// Return the length in bytes of the slice this `Block` manages.
    #[inline]
    #[expect(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Return a pointer to the memory this `Block` manages.
    ///
    /// The returned memory remains allocated until this `Block` is dropped.
    #[inline]
    pub fn bytes_mut(&self) -> *mut [u8] {
        self.bytes
    }
}

impl Drop for Block {
    fn drop(&mut self) {
        // Safety: We are in the `Drop` method, so `self.free` will
        // never be used after we return, and this method doesn't use
        // it either.
        let free = unsafe { ManuallyDrop::take(&mut self.free) };
        (free)(self.bytes);
    }
}

#[cfg(test)]
impl Block {
    pub fn from_vec(vec: Vec<u8>) -> Block {
        let slice = vec.into_boxed_slice();
        let slice_ptr = Box::into_raw(slice);
        let free = |slice_ptr| {
            // Safety: `slice_ptr` was obtained from a `Vec` in the first place.
            drop(Vec::from(unsafe { Box::from_raw(slice_ptr) }));
        };

        // Safety: `free` is designed to receive a `*mut [u8]` obtained from a
        // `Vec<u8>`.
        unsafe { Self::new(slice_ptr, free) }
    }
}

/// `Block` implements `Send`.
#[test]
fn block_is_send() {
    fn require_send<T: Send>(_t: T) {}
    let block = unsafe { Block::new(&mut [], |_bytes| ()) };
    require_send(block);
}

/// When a `Block` is dropped, the callback gets invoked.
#[test]
fn drop_called() {
    use std::sync::Arc;
    fn require_send<T: Send>(_t: T) {}

    let counter = Arc::new(());

    // Brute force the `Arc<()>` into a `*mut [u8]`.
    let bytes = Arc::into_raw(Arc::clone(&counter)) as *mut ();
    let bytes = std::ptr::slice_from_raw_parts_mut(bytes as *mut u8, 0);

    let free_bytes = move |bytes| {
        // Brute force the `*mut [u8]` back into an `Arc<()>`.
        let bytes = bytes as *mut u8 as *mut ();
        let clone_of_counter = unsafe { Arc::from_raw(bytes) };
        drop(clone_of_counter);
    };
    let block = unsafe { Block::new(bytes, free_bytes) };
    assert_eq!(Arc::strong_count(&counter), 2);
    require_send(block);
    assert_eq!(Arc::strong_count(&counter), 1);
}

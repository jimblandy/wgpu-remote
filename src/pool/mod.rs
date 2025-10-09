/*! A heap allocator for shared memory blocks.

This module defines the [`Pool`] type, which suballocates blocks from
some large underlying contiguous block of memory. It uses the [Binary
Buddy][bb] algorithm to place allocated blocks and coalesce free
ranges.

This crate uses a [`Sender`] implementation to create large blocks of
memory shared between client and server, and then uses [`Pool`] to
subdivide those large blocks up into individual buffers for mapping
wgpu [`Buffer`]s, large buffer writes, and so on.

## Why do we need another allocator?

There are plenty of heap allocators out there, but this crate has the
unusual constraint that the memory being managed may be writable by an
untrusted party. This means that we can't use free space to hold
memory management metadata like freelists, page headers, and so on.
All our metadata must live outside the underlying blocks of shared
memory.

One nice consequence of keeping the metadata separate is that the
allocator uses only one piece of unsafe code, for constructing the
sub-`Block` of the larger block we're dicing up. The rest of this
allocator simply operates on ordinary safe Rust types.

[`Sender`]: crate::transport::Sender
[`Buffer`]: https://docs.rs/wgpu/latest/wgpu/struct.Buffer.html
[bb]: https://en.wikipedia.org/wiki/Buddy_memory_allocation

*/

#![allow(unused_variables, dead_code)]

mod freemap;
mod slice_utils;
mod sparse_bit_set;

use freemap::FreeMap;
use slice_utils::{ptr_subslice, ptr_subslice_range};

use crate::Block;

use std::sync::{Arc, Mutex};

/// A large block of memory from which we can allocate smaller blocks.
///
/// Cloning a `Pool` produces a new reference to the same underlying memory
/// pool and allocation map.
#[derive(Clone)]
pub struct Pool {
    /// The actual map, shared by all `Block`s allocated from this `Pool`.
    //
    // If we don't care about sharing `Pool` between threads, this could
    // become an `Rc<RefCell<Inner>>`.
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    /// The overall block managed by this `Pool`.
    ///
    /// The [`Block`]s returned by [`Pool::allocate`] are subranges of this [`Block`].
    block: Block,

    /// A map of which portions of `block` are allocated and free.
    ///
    /// Every region of our [`Inner::block`] for which we have handed out a
    /// [`Block`] is marked as used in this freemap, until that `Block` is
    /// dropped.
    ///
    /// In order to satisfy the safety requirements of the [`Block`]s the
    /// allocator hands out, regions marked free in this map have no pointers
    /// into them, other than [`Inner::block`] above.
    freemap: FreeMap,
}

impl Pool {
    /// Create a `Pool` managing the given slice of memory.
    ///
    /// The memory `block` must be at least `1 << `[`SMALLEST_ORDER`] bytes long.
    ///
    /// [`SMALLEST_ORDER`]: freemap::SMALLEST_ORDER
    fn new(block: Block) -> Self {
        let freemap = FreeMap::new(block.len());

        let inner = Inner { block, freemap };

        Self {
            inner: Arc::new(Mutex::new(inner)),
        }
    }

    /// Allocate a block of at least `size` bytes,
    ///
    /// The bytes will be returned to the pool when the returned [`Block`] is
    /// dropped.
    //
    // You might think the guts of this should be a method on `Inner`, but the
    // `Block` we return needs to own a clone of the `Arc<Mutex>`, so this
    // actually does need to be a method on `Pool`.
    fn allocate(&self, size: usize) -> Option<Block> {
        let mut inner = self.inner.lock().unwrap();

        // Allocate a subrange of `inner.block`.
        let range = inner.freemap.allocate(size)?;
        let subrange = ptr_subslice(inner.block.bytes_mut(), range);

        // Build a closure to free that subrange.
        let free = {
            let inner_clone = Arc::clone(&self.inner);
            move |subrange| inner_clone.lock().unwrap().free(subrange)
        };

        // Safety:
        //
        // - `subrange` will live as long as `Block` does:
        //
        //   - The `free` closure owns a clone of `inner`.
        //   - Thus, `inner` will live at least until the closure is dropped.
        //   - Thus, `inner.block` will live until then.
        //   - Thus, `subrange` will live until then, since it is from `inner.block`.
        //
        // - Since this range was previously marked as available in
        //   `self.freemap`, but is now marked as allocated, nobody else should
        //   be referring to it.
        let block = unsafe { Block::new(subrange, free) };

        Some(block)
    }
}

impl Inner {
    fn free(&mut self, subblock: *mut [u8]) {
        let range = ptr_subslice_range(self.block.bytes_mut(), subblock);
        self.freemap.free(range);
    }
}

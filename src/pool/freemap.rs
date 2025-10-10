//! The defintion of the [`FreeMap`] type.

use super::sparse_bit_set::SparseBitSet;

use std::ops::Range;

/// The base 2 logarithm of the smallest block size we allocate.
pub const SMALLEST_ORDER: usize = 4;
pub const SMALLEST_SIZE: usize = 1 << SMALLEST_ORDER;

/// A map of the free and allocated regions within some [`Block`].
///
/// [`Block`]: crate::Block
pub struct FreeMap {
    /// A freelist for each block order.
    ///
    /// Let the "order" of a block be the base 2 logarithm of its size. So, for
    /// example, order 6 blocks are 64 bytes long, and order 9 blocks are 512
    /// bytes long.
    ///
    /// The `i`'th element of this vector is a [`SparseBitSet`] holding the
    /// indices of all free blocks of order `SMALLEST_ORDER + i`, viewing the
    /// overall block as an array of sub-blocks of that order (and ignoring all
    /// other orders). In other words, `j` is a member if the memory at offset
    /// `j << (SMALLEST_ORDER + i)` within the overall block is free.
    ///
    /// For example, assuming `SMALLEST_ORDER` is 4, `freelists[2]` contains the
    /// indices of all free blocks of order 6, with a size of 64 bytes. If
    /// `freelists[2]`'s only members are 2 and 7, then that means that the only
    /// 64-byte free blocks in `self.block` are at offsets 128 and 448 (2 * 64
    /// and 7 * 64).
    ///
    /// This is initialized with enough elements to cover all subblock sizes
    /// that could be allocated from `block`.
    //
    // # Rationale
    //
    // Why do we use `SparseBitSet` here, instead of just `Vec<usize>`?
    //
    // - This should usually be more compact than an actual list of
    //   `usize` offsets.
    //
    // - We want to keep the freelist sorted, so that we can look up arbitrary
    //   blocks without a full linear scan. With `SparseBitSet`, we avoid
    //   sliding vector elements back and forth when insertions and deletions
    //   can operate on an existing chunk.
    //
    // Why do we use `SparseBitSet` instead of a simple `BitSet`?
    //
    // - A flat `BitSet` could end up using a lot of memory for the smaller
    //   block sizes, even if there are only a few blocks free. A `SparseBitSet`
    //   should play better with our coalescing.
    freelists: Vec<SparseBitSet>,
}

impl FreeMap {
    pub fn new(pool_size: usize) -> FreeMap {
        assert!(pool_size >= SMALLEST_SIZE);
        let largest_order = pool_size.ilog2() as usize;

        // Build the vector of freelists, from the largest order to smallest.
        let mut freelists = Vec::with_capacity(largest_order - SMALLEST_ORDER + 1);
        // Offset of the next byte not covered by some freelist entry.
        let mut next_unlisted = 0;
        freelists.extend((SMALLEST_ORDER..=largest_order).rev().map(|order| {
            let mut freelist = SparseBitSet::default();
            // Can we carve a block of this order out of the remaining space?
            let block_size = 1 << order;
            if next_unlisted + block_size <= pool_size {
                freelist.insert(next_unlisted >> order);
                next_unlisted += block_size;
            }
            freelist
        }));

        // We should have reached the end of block, ignoring any slop
        // smaller than our smallest block at the end.
        assert!(next_unlisted + SMALLEST_SIZE > pool_size);

        // We built the freelists from the largest order to the smallest, but
        // it's stored in the opposite order.
        freelists.reverse();

        Self { freelists }
    }

    /// Allocate a range of at least `size` bytes,
    pub fn allocate(&mut self, size: usize) -> Option<Range<usize>> {
        let size = std::cmp::max(size, SMALLEST_SIZE);
        let requested_order = size.next_power_of_two().trailing_zeros() as usize;

        // Starting with the requested order, search through larger and larger
        // orders until we find a free block.
        let mut order = requested_order;

        let mut allocated_index = loop {
            // Find the freelist for `order`. If `order` is too large for this
            // freemap, then allocation has failed. Since we try successively
            // larger orders when smaller orders are exhausted, this will
            // eventually occur for an unsatisfiable request of any size.
            let freelist = self.freelists.get_mut(order - SMALLEST_ORDER)?;

            // Prefer the last member, to try to get stack-like behavior out of
            // the `SparseBitSet`, rather than favoring insertions and deletions
            // at the front.
            //
            // TODO: would it be better for locality to try to draw from the
            // chunk that has the fewest free blocks?
            if let Some(free) = freelist.largest_member() {
                // We found an available block. Mark it as in use.
                // TODO: we should have used pop_largest_member, which should
                // exist.
                assert!(freelist.remove(free));
                break free; // well, it's allocated now
            }

            // We have no free blocks in this order, so try the next larger.
            order += 1;
        };

        // We have marked the `allocated_index`'th block of order `order` as now
        // in use. Subdivide it as needed until it's of the requested order.
        while order > requested_order {
            order -= 1;
            allocated_index *= 2;

            // Since we've decremented `order`, `allocated_index` is now the
            // index of the left block in a pair of freshly allocated blocks of
            // that order. We only need one, so add the pair's right block to
            // this order's freelist.
            let right_block_index = allocated_index + 1;
            assert!(!self.freelists[order - SMALLEST_ORDER].insert(right_block_index));
        }

        // Now `allocated` refers to a freshly allocated block of the requested
        // order. Return its range.
        Some(allocated_index << order..(allocated_index + 1) << order)
    }

    pub fn free(&mut self, range: Range<usize>) {
        let len = range.end - range.start;
        let order = len.trailing_zeros() as usize;

        // Ensure that the binary buddy allocation invariants are respected.
        assert!(len.is_power_of_two());
        assert!(range.start & (len - 1) == 0); // range.start is multiple of len

        // Increase the order of this block as much as possible by coalescing it
        // with its buddies. This entails removing buddies from smaller orders
        // and then inserting the fully coalesced block into its final order.
        //
        // This loop should always terminate before `order` runs off the end of
        // `self.freelists`, because the sole block of the largest order never
        // has a buddy to coalesce with.
        let mut order = order;
        loop {
            let index = range.start >> order;
            let buddy = index ^ 1;

            // Can we coalesce this block with its buddy?
            let freelist = &mut self.freelists[order - SMALLEST_ORDER];
            if !freelist.remove(buddy) {
                // If not, then this is the fully coalesced free block.
                // TODO: this could be fused with the `remove` above
                freelist.insert(index);
                break;
            }

            order += 1;
            // Even though we've coalesced the block with its buddy, there's no
            // need to adjust `range.start`, because the `>> order` right shift
            // will get the right answer anyway. Any 1-bits will just get
            // dropped. And it's nicer to let `range` avoid being `mut`.
        }
    }
}

#[cfg(test)]
impl FreeMap {
    fn free_ranges(&self) -> Vec<Range<usize>> {
        self.freelists
            .iter()
            .enumerate()
            .flat_map(|(i, freelist)| {
                let order = i + SMALLEST_ORDER;
                freelist
                    .members()
                    .into_iter()
                    .map(move |m| m << order..(m + 1) << order)
            })
            .collect()
    }
}

#[test]
fn initial_freelists() {
    let freemap = FreeMap::new(208);
    assert_eq!(freemap.freelists.len(), 4);
    assert_eq!(freemap.freelists[0].members(), vec![192 / 16]);
    assert_eq!(freemap.freelists[1].members(), vec![]);
    assert_eq!(freemap.freelists[2].members(), vec![128 / 64]);
    assert_eq!(freemap.freelists[3].members(), vec![0]);

    let freemap = FreeMap::new(65);
    assert_eq!(freemap.freelists.len(), 3);
    assert!(freemap.freelists[0].members().is_empty());
    assert!(freemap.freelists[1].members().is_empty());
    assert_eq!(freemap.freelists[2].members(), vec![0]);

    let freemap = FreeMap::new(127);
    assert_eq!(freemap.free_ranges(), vec![96..112, 64..96, 0..64]);
}

#[test]
fn allocate_everything() {
    let mut freemap = FreeMap::new(65);
    assert_eq!(freemap.allocate(64), Some(0..64));
    assert_eq!(freemap.free_ranges(), vec![]);

    // exhaust
    assert_eq!(freemap.allocate(1), None);

    // return
    freemap.free(0..64);
    assert_eq!(freemap.free_ranges(), vec![0..64]);
}

#[test]
fn allocate_half() {
    let mut freemap = FreeMap::new(65);
    assert_eq!(freemap.allocate(32), Some(0..32));
    assert_eq!(freemap.free_ranges(), vec![32..64]);

    // exhaust
    assert_eq!(freemap.allocate(32), Some(32..64));
    assert_eq!(freemap.allocate(1), None);

    // return
    freemap.free(0..32);
    freemap.free(32..64);
    assert_eq!(freemap.free_ranges(), vec![0..64]);
}

#[test]
fn allocate_quarter() {
    let mut freemap = FreeMap::new(65);
    assert_eq!(freemap.allocate(16), Some(0..16));
    assert_eq!(freemap.free_ranges(), vec![16..32, 32..64]);

    // exhaust
    assert_eq!(freemap.allocate(32), Some(32..64));
    assert_eq!(freemap.allocate(16), Some(16..32));
    assert_eq!(freemap.allocate(1), None);
}

#[test]
fn allocate_tiny() {
    let mut freemap = FreeMap::new(65);
    assert_eq!(freemap.allocate(1), Some(0..16));
}

#[test]
fn allocate_two_quarters() {
    let mut freemap = FreeMap::new(65);
    assert_eq!(freemap.allocate(16), Some(0..16));
    assert_eq!(freemap.allocate(16), Some(16..32));
    assert_eq!(freemap.free_ranges(), vec![32..64]);

    // exhaust
    assert_eq!(freemap.allocate(32), Some(32..64));
    assert_eq!(freemap.allocate(1), None);
}

#[test]
fn allocate_one_quarter_one_eighth() {
    let mut freemap = FreeMap::new(129);
    assert_eq!(freemap.allocate(32), Some(0..32));
    assert_eq!(freemap.allocate(16), Some(32..48));
    assert_eq!(freemap.free_ranges(), vec![48..64, 64..128]);

    let mut freemap = FreeMap::new(129);
    assert_eq!(freemap.allocate(16), Some(0..16));
    assert_eq!(freemap.allocate(32), Some(32..64));
    assert_eq!(freemap.free_ranges(), vec![16..32, 64..128]);

    // exhaust
    assert_eq!(freemap.allocate(64), Some(64..128));
    assert_eq!(freemap.allocate(16), Some(16..32));
    assert_eq!(freemap.allocate(1), None);

    // return
    freemap.free(32..64);
    assert_eq!(freemap.free_ranges(), vec![32..64]);
    freemap.free(64..128);
    assert_eq!(freemap.free_ranges(), vec![32..64, 64..128]);
    freemap.free(16..32);
    assert_eq!(freemap.free_ranges(), vec![16..32, 32..64, 64..128]);
    freemap.free(0..16);
    assert_eq!(freemap.free_ranges(), vec![0..128]);
}

#[test]
fn allocate_one_half_one_eighth() {
    let mut freemap = FreeMap::new(129);
    assert_eq!(freemap.allocate(64), Some(0..64));
    assert_eq!(freemap.allocate(16), Some(64..80));
    assert_eq!(freemap.free_ranges(), vec![80..96, 96..128]);

    let mut freemap = FreeMap::new(129);
    assert_eq!(freemap.allocate(16), Some(0..16));
    assert_eq!(freemap.allocate(64), Some(64..128));
    assert_eq!(freemap.free_ranges(), vec![16..32, 32..64]);

    // exhaust
    assert_eq!(freemap.allocate(32), Some(32..64));
    assert_eq!(freemap.allocate(16), Some(16..32));
    assert_eq!(freemap.allocate(1), None);
}

#[test]
fn allocate_free_one_eighth() {
    let mut freemap = FreeMap::new(128);
    assert_eq!(freemap.free_ranges(), vec![0..128]);
    assert_eq!(freemap.allocate(16), Some(0..16));
    assert_eq!(freemap.free_ranges(), vec![16..32, 32..64, 64..128]);
    freemap.free(0..16);
    assert_eq!(freemap.free_ranges(), vec![0..128]);
}

#[test]
fn allocate_free_one_eighth_no_coalesce() {
    let mut freemap = FreeMap::new(127);
    assert_eq!(freemap.free_ranges(), vec![96..112, 64..96, 0..64]);
    assert_eq!(freemap.allocate(16), Some(96..112));
    assert_eq!(freemap.free_ranges(), vec![64..96, 0..64]);
    freemap.free(96..112);
    assert_eq!(freemap.free_ranges(), vec![96..112, 64..96, 0..64]);
}

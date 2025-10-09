//! Definition of the `SparseBitSet`  type.
#![allow(dead_code)]

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::num::NonZeroU64;

/// A set of `usize` values, represented as a sparse list of bitmaps.
#[derive(Debug, Default)]
pub struct SparseBitSet {
    /// A tree of chunks, sorted by initial bit number.
    ///
    /// If the value stored at key `i` has its `j`'th bit set, then that means
    /// that `i + j` is a member of the set. Every key is a multiple of
    /// `BITS_PER_CHUNK`.
    ///
    /// Originally, I had this as a sorted Vec. But BTreeMap seems to combine
    /// everything I wanted from a Vec with better behavior when the set gets
    /// large.
    ///
    /// Each BTreeMap node stores a run of key/value pairs as a sorted array,
    /// whose size is chosen to balance the locality and prefetch-friendliness
    /// of a linear search in an array with the joy of skipping irrelevant nodes
    /// entirely of a tree.
    chunks: BTreeMap<usize, NonZeroU64>,
}

const BITS_PER_CHUNK: usize = std::mem::size_of::<u64>() * 8;
const INDEX_MASK: usize = BITS_PER_CHUNK - 1;

impl SparseBitSet {
    /// Add `n` to the set. Return true if `n` was previously a member.
    pub fn insert(&mut self, n: usize) -> bool {
        let (chunk_start, bit) = Self::split_element(n);
        match self.chunks.entry(chunk_start) {
            Entry::Vacant(vacant) => {
                vacant.insert(bit);
                false
            }
            Entry::Occupied(mut occupied) => {
                let occupied = occupied.get_mut();
                let previously = occupied.get() & bit.get() != 0;
                *occupied |= bit;
                previously
            }
        }
    }

    pub fn remove(&mut self, n: usize) -> bool {
        let (chunk_start, bit) = Self::split_element(n);
        match self.chunks.entry(chunk_start) {
            Entry::Vacant(_) => false,
            Entry::Occupied(mut occupied) => {
                let entry_bits = occupied.get_mut();
                let previously = entry_bits.get() & bit.get() != 0;
                if let Some(new_bits) = NonZeroU64::new(entry_bits.get() & !bit.get()) {
                    *entry_bits = new_bits;
                } else {
                    occupied.remove();
                }
                previously
            }
        }
    }

    pub fn contains(&self, n: usize) -> bool {
        let (chunk_start, bit) = Self::split_element(n);
        self.chunks
            .get(&chunk_start)
            .is_some_and(|bits| bits.get() & bit.get() != 0)
    }

    /// Return the largest member, or `None` if the set is empty.
    pub fn largest_member(&self) -> Option<usize> {
        let (start_bit, bits) = self.chunks.last_key_value()?;
        let greatest_set_bit_index = 63 - bits.leading_zeros() as usize;
        Some(start_bit + greatest_set_bit_index)
    }

    /// Split an element number into a chunk start and the bitmask for
    /// `n` within that chunk.
    fn split_element(n: usize) -> (usize, NonZeroU64) {
        // Note that, because INDEX_MASK is one less than the number of bits in
        // a `u64`, (n & INDEX_MASK) is always a valid shift distance.
        (
            n & !INDEX_MASK,
            NonZeroU64::new(1 << (n & INDEX_MASK)).unwrap(),
        )
    }
}

#[cfg(test)]
impl SparseBitSet {
    pub fn members(&self) -> Vec<usize> {
        self.chunks
            .iter()
            .flat_map(|(&start_bit, &bits)| {
                (0..63)
                    .filter(move |&i| bits.get() & 1 << i != 0)
                    .map(move |i| start_bit + i)
            })
            .collect()
    }
}

#[test]
fn empty() {
    let empty = SparseBitSet::default();
    assert!(!empty.contains(0));
    assert!(!empty.contains(63));
    assert!(!empty.contains(64));
    assert!(empty.largest_member().is_none());
}

#[test]
fn one_chunk_at_zero() {
    let mut set = SparseBitSet::default();
    assert!(!set.insert(0));
    assert!(!set.insert(63));
    assert!(!set.insert(1));

    assert!(set.insert(63));

    assert_eq!(set.largest_member(), Some(63));
    assert_eq!(
        (0..256).filter(|&n| set.contains(n)).collect::<Vec<_>>(),
        vec![0, 1, 63]
    );
}

#[test]
fn one_chunk_later() {
    let mut set = SparseBitSet::default();
    set.insert(256);
    set.insert(270);
    set.insert(256 + 63);

    assert_eq!(
        (0..500).filter(|&n| set.contains(n)).collect::<Vec<_>>(),
        vec![256, 270, 256 + 63]
    );
}

#[test]
fn two_chunks() {
    let mut set = SparseBitSet::default();
    set.insert(0);
    set.insert(63);
    set.insert(1);
    set.insert(256);

    eprintln!("{set:?}");
    assert_eq!(
        (0..300).filter(|&n| set.contains(n)).collect::<Vec<_>>(),
        vec![0, 1, 63, 256]
    );
}

#[test]
fn remove() {
    let mut set = SparseBitSet::default();
    assert!(!set.insert(65));
    assert!(!set.insert(256));
    assert!(!set.remove(64));
    assert!(set.remove(65));

    eprintln!("{set:?}");
    assert_eq!(
        (0..300).filter(|&n| set.contains(n)).collect::<Vec<_>>(),
        vec![256]
    );
}

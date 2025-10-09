/*! Process isolation for wgpu.

This crate implements a backend for `wgpu` that forwards operations
across an inter-process communication channel, so that the actual GPU
access takes place in a different process from the code driving the
`wgpu` API. You can use this to isolate GPU access to improve security
or robustness, or perhaps, in the other direction, to isolate the code
that is using wgpu.

Code using the `wgpu` API with this crate as a backend is considered
to be the "client", and the code responding to its requests and
driving the GPU is considered the "server".

The client and server communicate via implementations of the
[`transport`] module's [`Sender`] and [`Receiver`] traits. These are
meant to be easily implemented on most platforms, or integrated with
an application's existing IPC mechanisms. They provide a low level
interface, with methods for creating blocks of shared memory, and
exchanging messages held in that memory. This crate also provides a
few basic implementations of `Sender` and `Receiver`; see the
[`transport`] module documentation for details.

This crate requires the client and server to be able to share memory.
It is probably not suitable for allowing the two parties to run on
entirely different machines and communicate over a network.

[`Sender`]: transport::Sender
[`Receiver`]: transport::Receiver

*/

mod block;
mod pool;
pub mod transport;

pub use block::Block;
pub use pool::Pool;

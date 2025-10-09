//! Transports based on Unix facilities.
#![allow(dead_code)]

/// A [`Sender`] implementation based on mmap and Unix domain sockets.
///
/// This type implements [`transport::Sender`] based on a Unix domain socket
/// connection to the counterpart that you supply. It uses the [`memfd_create`]
/// system call to create shared memory segments, and then uses [AF_UNIX]'s
/// `SCM_RIGHTS` ancillary message to pass those file descriptors across the
/// socket to the counterpart.
///
/// [`transport::Sender`]: crate::transport::Sender
/// [`memfd_create`]: https://man7.org/linux/man-pages/man2/memfd_create.2.html
/// [AF_UNIX]: https://man7.org/linux/man-pages/man7/unix.7.html
struct Sender {
    /// The connection to the counterpart.
    socket: std::os::fd::OwnedFd,

    /// The id of the next shared memory segment we'll allocate.
    next_shmem_id: std::sync::atomic::AtomicUsize,
}


/*! Exercise sending shared memory via Unix domain sockets.

This isn't really a test of the `wgpu-remote` crate's functionality; it's a
verification that the system actually behaves as `wgpu-remote` expects.

If it turns out that all Unix systems behave identically, then this is a waste
of time. But if it turns out that there are bugs, quirks, or limits, then this
serves to detect and document them.

Indeed, the behavior of `sendmsg` and `recvmsg` around empty messages is a bit
surprising.

*/

use std::os::fd::{self, AsRawFd, FromRawFd};

use nix::sys::{memfd, mman, socket, wait};
use nix::unistd;

fn main() {
    send_one_shmem_fd();
    no_bytes_no_rights(false);
    no_bytes_no_rights(true);
}

/// Try sending a single shared memory FD from one process to another
/// over a Unix domain socket.
fn send_one_shmem_fd() {
    eprint!("test send_one_shmem_fd... ");
    let (sender_socket, receiver_socket) = socket::socketpair(
        socket::AddressFamily::Unix,
        socket::SockType::Stream,
        None,
        socket::SockFlag::empty(),
    ).unwrap();

    // Start the sender.
    // Safety: we are single-threaded.
    let fork = unsafe { unistd::fork() }.unwrap();
    let unistd::ForkResult::Parent { child: sender_pid } = fork else {
        drop(receiver_socket);
        send_one_shmem_fd_sender(sender_socket);
        // never returns
    };

    // Start the receiver.
    // Safety: we are single-threaded.
    let fork = unsafe { unistd::fork() }.unwrap();
    let unistd::ForkResult::Parent { child: receiver_pid } = fork else {
        drop(sender_socket);
        send_one_shmem_fd_receiver(receiver_socket);
        // never returns
    };

    drop(sender_socket);
    drop(receiver_socket);

    let status = wait::waitpid(sender_pid, None).unwrap();
    assert_eq!(status, wait::WaitStatus::Exited(sender_pid, 42));

    let status = wait::waitpid(receiver_pid, None).unwrap();
    assert_eq!(status, wait::WaitStatus::Exited(receiver_pid, 43));

    eprintln!("passed");
}

const LEN: usize = 10;

fn send_one_shmem_fd_sender(socket: fd::OwnedFd) -> ! {
    // Create a file descriptor referring to zeroed memory.
    //
    // The `nix` source code suggests `memfd_create` is available on
    // Linux, Android, and FreeBSD.
    let mem_fd = memfd::memfd_create(
        c"unix-send-shmem",
        memfd::MemFdCreateFlag::empty(),
    ).unwrap();

    // Set the file's size.
    unistd::ftruncate(&mem_fd, LEN as _).unwrap();

    // Map the memory file descriptor into our address space.
    //
    // Safety: We're not requesting an address, our offset is aligned,
    // and our flags are reasonable.
    let mapped = unsafe {
        mman::mmap(
            None,
            std::num::NonZeroUsize::new(LEN).unwrap(),
            mman::ProtFlags::PROT_READ | mman::ProtFlags::PROT_WRITE,
            mman::MapFlags::MAP_SHARED,
            &mem_fd,
            0,
        ).unwrap()
    };

    // Write a message to the memory.
    //
    // Safety: the memory is there and zeroed, and `u8` has no
    // requirement alignments.
    let mapped = unsafe { mapped.cast::<[u8; LEN]>().as_mut() };
    mapped.copy_from_slice(b"Greetings!");

    // Send the file descriptor to the other process.
    //
    // The unix(7) man page for Linux says:
    //
    // > At least one byte of real data should be sent when sending
    // > ancillary data. On Linux, this is required to successfully send
    // > ancillary data over a UNIX domain stream socket.
    socket::sendmsg::<()>(
        socket.as_raw_fd(),
        &[std::io::IoSlice::new(b"X")], // one byte of "real" data
        &[socket::ControlMessage::ScmRights(&[mem_fd.as_raw_fd()])],
        socket::MsgFlags::empty(),
        None, // address
    ).unwrap();

    std::process::exit(42);
}

fn send_one_shmem_fd_receiver(socket: fd::OwnedFd) -> ! {
    // Now do a recvmsg that accepts one byte of real data.
    let mut data_buf = [0_u8; 1];
    let mut slices = [std::io::IoSliceMut::new(&mut data_buf)];
    let mut fd_buf = nix::cmsg_space!(fd::RawFd);
    let msg = socket::recvmsg::<()>(
        socket.as_raw_fd(),
        &mut slices, // iov (ordinary bytes)
        Some(&mut fd_buf), // cmsg_buffer
        socket::MsgFlags::empty(),
    ).unwrap();

    // The `recvmsg` call should have had a large enough `cmsg_buffer` to hold
    // all the file descriptors.
    assert!(!msg.flags.contains(socket::MsgFlags::MSG_CTRUNC));

    // Get the memory file descriptor out of the message.
    assert_eq!(msg.bytes, 1);
    let mut cmsgs = msg.cmsgs().unwrap();
    let mut fds;
    match cmsgs.next() {
        Some(socket::ControlMessageOwned::ScmRights(f)) => { fds = f; }
        Some(other) => panic!("Got unexpected control message: {other:#?}"),
        None => panic!("didn't get any control messages"),
    }
    assert_eq!(cmsgs.next(), None);
    let raw_mem_fd = fds.pop().unwrap();
    // Safety: we just got this fd from a control message, so it should be open.
    let mem_fd = unsafe { fd::OwnedFd::from_raw_fd(raw_mem_fd) };
    assert!(fds.is_empty());

    // Check the byte that was transmitted.
    assert_eq!(&slices[0][..1], b"X");

    // Map the memory file descriptor into our address space.
    //
    // Safety: We're not requesting an address, our offset is aligned,
    // and our flags are reasonable.
    let mapped = unsafe {
        mman::mmap(
            None,
            std::num::NonZeroUsize::new(LEN).unwrap(),
            mman::ProtFlags::PROT_READ,
            mman::MapFlags::MAP_SHARED,
            &mem_fd,
            0,
        ).unwrap()
    };

    // Check the message in the memory.
    //
    // Safety: the memory is there and initialized, and `u8` has no
    // requirement alignments.
    let mapped = unsafe { mapped.cast::<[u8; LEN]>().as_ref() };
    assert_eq!(mapped, b"Greetings!");

    std::process::exit(43);
}

/// Verify that we can't receive a file descriptor unless we actually send some
/// bytes along with it.
///
/// The unix(7) man page for Linux says:
///
/// > At least one byte of real data should be sent when sending
/// > ancillary data. On Linux, this is required to successfully send
/// > ancillary data over a UNIX domain stream socket.
///
/// It further explains:
///
/// > When receiving from a stream socket, ancillary data forms a kind of
/// > barrier for the received data. For example, suppose that the sender
/// > transmits as follows:
/// >
/// > 1)  sendmsg(2) of four bytes, with no ancillary data.
/// > 2)  sendmsg(2) of one byte, with ancillary data.
/// > 3)  sendmsg(2) of four bytes, with no ancillary data.
/// >
/// > Suppose that the receiver now performs recvmsg(2) calls each with a buffer
/// > size of 20 bytes. The first call will receive five bytes of data, along
/// > with the ancillary data sent by the second sendmsg(2) call. The next call
/// > will receive the remaining four bytes of data.
///
/// The behavior we observe in this test is:
///
/// - a `sendmsg` that sends a file descriptor, but no ordinary bytes, is
///   basically a no-op. The receiver will even see bytes written later on the
///   socket, but the file descriptor never arrives.
///
/// - Sending one byte along with the file descriptor will cause the `recvmsg`
///   call that receives that byte to receive both that byte and the file
///   descriptor. The text above seems to further promise that that `recvmsg`
///   call won't see any subsequent bytes, but we don't test that.
fn no_bytes_no_rights(send_a_byte: bool) {
    eprint!("test no_bytes_no_rights(send_a_byte = {send_a_byte:?})... ");

    // Create a pipe. We'll (try to) send the reading end over the
    // socket.
    let (pipe_read, pipe_write) = unistd::pipe().unwrap();

    // Write some data to the pipe, so that in the `send_a_byte` case
    // we can verify that the receiver got the right fd. Surely this
    // will not overflow the pipe's buffer and deadlock.
    unistd::write(&pipe_write, b"unlikely").unwrap();

    // Create a Unix domain socket that we can send the reading side
    // of the pipe across.
    let (socket_read, socket_write) = socket::socketpair(
        socket::AddressFamily::Unix,
        socket::SockType::Stream,
        None,
        socket::SockFlag::empty(),
    ).unwrap();
    
    // Send the file descriptor to the other process, *but do not send
    // any bytes* unless `send_a_byte` is true.
    let iov: &[std::io::IoSlice] = if send_a_byte {
        &[std::io::IoSlice::new(b"X")]
    } else {
        &[]
    };
    socket::sendmsg::<()>(
        socket_write.as_raw_fd(),
        iov,
        &[socket::ControlMessage::ScmRights(&[pipe_read.as_raw_fd()])],
        socket::MsgFlags::empty(),
        None, // address
    ).unwrap();

    // Write another byte to the socket, so that the reader won't block.
    unistd::write(&socket_write, b"Y").unwrap();

    // Now do a recvmsg, hoping to receive that right.
    let mut data_buf = [0_u8; 1];
    let mut slices = [std::io::IoSliceMut::new(&mut data_buf)];
    let mut fd_buf = nix::cmsg_space!(fd::RawFd);
    let msg = socket::recvmsg::<()>(
        socket_read.as_raw_fd(),
        &mut slices, // iov (ordinary bytes)
        Some(&mut fd_buf), // cmsg_buffer
        socket::MsgFlags::empty(),
    ).unwrap();

    // The `recvmsg` call should have had a large enough `cmsg_buffer` to hold
    // all the file descriptors.
    assert!(!msg.flags.contains(socket::MsgFlags::MSG_CTRUNC));

    if send_a_byte {
        assert_eq!(msg.bytes, 1);

        let mut cmsgs = msg.cmsgs().unwrap();
        let mut fds;
        match cmsgs.next() {
            Some(socket::ControlMessageOwned::ScmRights(f)) => { fds = f; }
            Some(other) => panic!("Got unexpected control message: {other:#?}"),
            None => panic!("didn't get any control messages"),
        }
        assert_eq!(cmsgs.next(), None);
        let raw_mem_fd = fds.pop().unwrap();
        assert!(fds.is_empty());
        // Safety: we just got this fd from a control message, so it should be open.
        let received_pipe_read = unsafe { fd::OwnedFd::from_raw_fd(raw_mem_fd) };

        // Check the byte that was transmitted.
        assert_eq!(&slices[0][..1], b"X");

        // Read data from the received file descriptor, to verify that it is
        // indeed the pipe's reading end.
        let mut buf2 = [0_u8; 8];
        unistd::read(received_pipe_read.as_raw_fd(), &mut buf2).unwrap();
        assert_eq!(&buf2, b"unlikely");

        // Read the second byte transmitted by the `write` call on `socket_write`.
        let msg = socket::recvmsg::<()>(
            socket_read.as_raw_fd(),
            &mut slices, // iov (ordinary bytes)
            Some(&mut fd_buf), // cmsg_buffer
            socket::MsgFlags::empty(),
        ).unwrap();

        assert_eq!(msg.bytes, 1);

        // Check that no control messages were received by this `recvmsg`; this
        // was just an ordinary `write`.
        let mut cmsgs = msg.cmsgs().unwrap();
        assert_eq!(cmsgs.next(), None);

        // Check the byte that was transmitted.
        assert_eq!(&slices[0][..1], b"Y");
    } else {
        assert_eq!(msg.bytes, 1);

        // Check that no control messages were received, even though the call
        // succeeded, and we read a subsequently written byte.
        let mut cmsgs = msg.cmsgs().unwrap();
        assert_eq!(cmsgs.next(), None);

        // Check the byte that was transmitted.
        assert_eq!(&slices[0][..1], b"Y");
    }

    eprintln!("passed");
}

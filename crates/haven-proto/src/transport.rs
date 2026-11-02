//! Transports. A socket for the daemon, an in-process channel for standalone.
//!
//! `docs/PLAN.md` §4.1: standalone is not a special case, it is a transport.
//! Keeping it behind this trait means attach/detach logic cannot hide bugs in a
//! mode nobody exercises.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use serde::{Deserialize, Serialize};

use crate::{ClientMsg, ServerMsg};

#[derive(Debug, PartialEq, Eq)]
pub enum TransportError {
    /// The peer is gone. Always recoverable at the UI level: a client reports
    /// "server went away", a daemon drops the client.
    Disconnected,
    Protocol(String),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disconnected => write!(f, "transport disconnected"),
            Self::Protocol(m) => write!(f, "protocol error: {m}"),
        }
    }
}

impl std::error::Error for TransportError {}

pub trait Transport: std::fmt::Debug {
    fn send(&mut self, msg: ClientMsg) -> Result<(), TransportError>;
    /// Non-blocking. `None` means "nothing right now", not "finished".
    fn try_recv(&mut self) -> Result<Option<ServerMsg>, TransportError>;
}

/// In-process transport over channels: `--solo` mode.
#[derive(Debug)]
pub struct LocalTransport {
    to_server: Sender<ClientMsg>,
    from_server: Receiver<ServerMsg>,
}

impl LocalTransport {
    /// Returns the client half plus the daemon-side ends.
    pub fn pair() -> (Self, Sender<ServerMsg>, Receiver<ClientMsg>) {
        let (to_server, server_rx) = std::sync::mpsc::channel();
        let (server_tx, from_server) = std::sync::mpsc::channel();
        (Self { to_server, from_server }, server_tx, server_rx)
    }
}

impl Transport for LocalTransport {
    fn send(&mut self, msg: ClientMsg) -> Result<(), TransportError> {
        self.to_server.send(msg).map_err(|_| TransportError::Disconnected)
    }

    fn try_recv(&mut self) -> Result<Option<ServerMsg>, TransportError> {
        match self.from_server.try_recv() {
            Ok(msg) => Ok(Some(msg)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(TransportError::Disconnected),
        }
    }
}

/// A framed connection over a byte stream, used by both halves of the
/// protocol: the client's [`SocketTransport`] and the daemon's per-client
/// connection are the same framing code with the message types swapped.
#[derive(Debug)]
pub struct Framed<S> {
    pub stream: S,
    inbox: Vec<u8>,
    outbox: Vec<u8>,
    /// Set once the peer closes: reads stop, but queued output still flushes.
    closed: bool,
}

impl<S: Read + Write> Framed<S> {
    pub fn new(stream: S) -> Self {
        Self { stream, inbox: Vec::new(), outbox: Vec::new(), closed: false }
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Queue a message. Writes are buffered so a slow peer cannot block the
    /// daemon's event loop mid-frame.
    pub fn queue<T: Serialize>(&mut self, msg: &T) -> Result<(), TransportError> {
        let bytes =
            crate::frame::encode(msg).map_err(|e| TransportError::Protocol(e.to_string()))?;
        self.outbox.extend_from_slice(&bytes);
        Ok(())
    }

    /// Push as much of the outbox as the peer will take right now.
    pub fn flush(&mut self) -> Result<(), TransportError> {
        while !self.outbox.is_empty() {
            match self.stream.write(&self.outbox) {
                Ok(0) => return Err(TransportError::Disconnected),
                Ok(n) => {
                    self.outbox.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(TransportError::Disconnected),
            }
        }
        let _ = self.stream.flush();
        Ok(())
    }

    pub fn has_pending_output(&self) -> bool {
        !self.outbox.is_empty()
    }

    /// Read whatever is available into the inbox.
    pub fn fill(&mut self) -> Result<(), TransportError> {
        let mut buf = [0u8; 16384];
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => {
                    self.closed = true;
                    return Ok(());
                }
                Ok(n) => self.inbox.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.closed = true;
                    return Ok(());
                }
            }
        }
    }

    /// Pop one complete message, if the inbox holds one.
    pub fn next_message<T: for<'a> Deserialize<'a>>(
        &mut self,
    ) -> Result<Option<T>, TransportError> {
        match crate::frame::decode::<T>(&self.inbox) {
            Ok(Some((msg, used))) => {
                self.inbox.drain(..used);
                Ok(Some(msg))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(TransportError::Protocol(e.to_string())),
        }
    }
}

/// The client's end of a unix socket.
#[derive(Debug)]
pub struct SocketTransport {
    framed: Framed<UnixStream>,
}

impl SocketTransport {
    /// Connect and switch to non-blocking: the client's event loop must never
    /// block on the daemon, or a wedged daemon freezes the UI.
    pub fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        let stream = UnixStream::connect(path)?;
        stream.set_nonblocking(true)?;
        Ok(Self { framed: Framed::new(stream) })
    }

    /// Adopt an already-connected stream (a socket pair in tests, or a
    /// socket-activated fd).
    pub fn from_stream(stream: UnixStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self { framed: Framed::new(stream) })
    }

    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.framed.stream.as_fd()
    }
}

impl Transport for SocketTransport {
    fn send(&mut self, msg: ClientMsg) -> Result<(), TransportError> {
        self.framed.queue(&msg)?;
        self.framed.flush()
    }

    fn try_recv(&mut self) -> Result<Option<ServerMsg>, TransportError> {
        self.framed.fill()?;
        match self.framed.next_message()? {
            Some(msg) => Ok(Some(msg)),
            None if self.framed.is_closed() => Err(TransportError::Disconnected),
            None => Ok(None),
        }
    }
}

/// A transport that replays a canned script. The whole client loop is testable
/// with no daemon, no socket and no shell.
#[derive(Debug, Default)]
pub struct ScriptedTransport {
    pub incoming: VecDeque<ServerMsg>,
    pub sent: Vec<ClientMsg>,
    pub disconnect_after: Option<usize>,
}

impl ScriptedTransport {
    pub fn new(incoming: impl IntoIterator<Item = ServerMsg>) -> Self {
        Self { incoming: incoming.into_iter().collect(), sent: Vec::new(), disconnect_after: None }
    }
}

impl Transport for ScriptedTransport {
    fn send(&mut self, msg: ClientMsg) -> Result<(), TransportError> {
        if Some(self.sent.len()) == self.disconnect_after {
            return Err(TransportError::Disconnected);
        }
        self.sent.push(msg);
        Ok(())
    }

    fn try_recv(&mut self) -> Result<Option<ServerMsg>, TransportError> {
        Ok(self.incoming.pop_front())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PaneId, SessionId, PROTOCOL_VERSION};

    #[test]
    fn framing_over_a_socket_pair_carries_messages_both_ways() {
        let (a, b) = UnixStream::pair().unwrap();
        a.set_nonblocking(true).unwrap();
        b.set_nonblocking(true).unwrap();
        let mut client = SocketTransport { framed: Framed::new(a) };
        let mut server: Framed<UnixStream> = Framed::new(b);

        client.send(ClientMsg::Detach).unwrap();
        server.fill().unwrap();
        assert_eq!(server.next_message::<ClientMsg>().unwrap(), Some(ClientMsg::Detach));
        assert_eq!(server.next_message::<ClientMsg>().unwrap(), None);

        server
            .queue(&ServerMsg::Hello { protocol: PROTOCOL_VERSION, session: SessionId(2) })
            .unwrap();
        server.flush().unwrap();
        assert!(matches!(client.try_recv().unwrap(), Some(ServerMsg::Hello { .. })));
        assert_eq!(client.try_recv().unwrap(), None, "nothing pending is not an error");
    }

    #[test]
    fn a_message_split_across_reads_is_reassembled() {
        let (a, b) = UnixStream::pair().unwrap();
        b.set_nonblocking(true).unwrap();
        let big = ServerMsg::Title { pane: PaneId(1), title: "x".repeat(5000) };
        let bytes = crate::frame::encode(&big).unwrap();

        let mut framed: Framed<UnixStream> = Framed::new(b);
        let mut writer = a;
        // Write the frame in two pieces, as a socket will.
        let (head, tail) = bytes.split_at(bytes.len() / 3);
        writer.write_all(head).unwrap();
        framed.fill().unwrap();
        assert_eq!(framed.next_message::<ServerMsg>().unwrap(), None, "partial frame");
        writer.write_all(tail).unwrap();
        framed.fill().unwrap();
        assert_eq!(framed.next_message::<ServerMsg>().unwrap(), Some(big));
    }

    #[test]
    fn a_closed_peer_surfaces_as_a_disconnect() {
        let (a, b) = UnixStream::pair().unwrap();
        a.set_nonblocking(true).unwrap();
        let mut client = SocketTransport { framed: Framed::new(a) };
        drop(b);
        assert_eq!(client.try_recv(), Err(TransportError::Disconnected));
    }

    #[test]
    fn local_transport_carries_both_directions() {
        let (mut client, server_tx, server_rx) = LocalTransport::pair();
        client.send(ClientMsg::Detach).unwrap();
        assert_eq!(server_rx.try_recv().unwrap(), ClientMsg::Detach);

        server_tx
            .send(ServerMsg::Hello { protocol: PROTOCOL_VERSION, session: SessionId(1) })
            .unwrap();
        assert!(matches!(client.try_recv().unwrap(), Some(ServerMsg::Hello { .. })));
        assert_eq!(client.try_recv().unwrap(), None, "empty is not an error");
    }

    #[test]
    fn dropping_the_server_half_is_a_disconnect_not_a_panic() {
        let (mut client, server_tx, server_rx) = LocalTransport::pair();
        drop(server_rx);
        assert_eq!(client.send(ClientMsg::Detach), Err(TransportError::Disconnected));
        drop(server_tx);
        assert_eq!(client.try_recv(), Err(TransportError::Disconnected));
    }

    #[test]
    fn scripted_transport_records_what_the_client_sent() {
        let mut t = ScriptedTransport::new([ServerMsg::Bell { pane: PaneId(3) }]);
        assert!(matches!(t.try_recv().unwrap(), Some(ServerMsg::Bell { .. })));
        assert_eq!(t.try_recv().unwrap(), None);
        t.send(ClientMsg::Detach).unwrap();
        assert_eq!(t.sent, vec![ClientMsg::Detach]);
    }
}

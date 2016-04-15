use buffer::Buffer;
use buffer::BufferRef;
use buffer::with_buffer;
use connection::Connection;
use connection::ReceiveChunk;
use connection;
use linear_map::LinearMap;
use linear_map;
use protocol::ConnectedPacket;
use protocol::ConnectedPacketType;
use protocol::ControlPacket;
use protocol::Packet;
use protocol;
use std::hash::Hash;
use std::iter;
use std::ops;

pub use connection::Error;

pub trait Callback<A: Address> {
    type Error;
    fn send(&mut self, addr: A, data: &[u8]) -> Result<(), Self::Error>;
}

pub trait Address: Copy + Eq + Hash + Ord { }
impl<A: Copy + Eq + Hash + Ord> Address for A { }

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PeerId(pub u32);

impl PeerId {
    fn get_and_increment(&mut self) -> PeerId {
        let old = *self;
        self.0 = self.0.wrapping_add(1);
        old
    }
}

#[derive(Clone, Copy)]
pub enum Id<A: Address> {
    Peer(PeerId),
    Address(A),
}

struct Peer<A: Address> {
    conn: Connection,
    addr: A,
}

impl<A: Address> Peer<A> {
    fn new(addr: A) -> Peer<A> {
        Peer {
            conn: Connection::new(),
            addr: addr,
        }
    }
}

struct Peers<A: Address> {
    peers: LinearMap<PeerId, Peer<A>>,
    next_peer_id: PeerId,
}

impl<A: Address> Peers<A> {
    fn new() -> Peers<A> {
        Peers {
            peers: LinearMap::new(),
            next_peer_id: PeerId(0),
        }
    }
    fn new_peer(&mut self, addr: A) -> (PeerId, &mut Peer<A>) {
        // FIXME: Work around missing non-lexical borrows.
        // TODO: Find issue number for this.
        let raw_self: *mut Peers<A> = self;
        unsafe {
            loop {
                let peer_id = self.next_peer_id.get_and_increment();
                if let linear_map::Entry::Vacant(v) = (*raw_self).peers.entry(peer_id) {
                    return (peer_id, v.insert(Peer::new(addr)));
                }
            }
        }
    }
    fn remove_peer(&mut self, pid: PeerId) {
        self.peers.remove(&pid).unwrap_or_else(|| panic!("invalid pid"));
    }
    fn pid_from_addr(&mut self, addr: A) -> Option<PeerId> {
        for (&pid, p) in self.peers.iter() {
            if p.addr == addr {
                return Some(pid);
            }
        }
        None
    }
    fn get(&self, pid: PeerId) -> Option<&Peer<A>> {
        self.peers.get(&pid)
    }
    fn get_mut(&mut self, pid: PeerId) -> Option<&mut Peer<A>> {
        self.peers.get_mut(&pid)
    }
}

impl<A: Address> ops::Index<PeerId> for Peers<A> {
    type Output = Peer<A>;
    fn index(&self, pid: PeerId) -> &Peer<A> {
        self.get(pid).unwrap_or_else(|| panic!("invalid pid"))
    }
}

impl<A: Address> ops::IndexMut<PeerId> for Peers<A> {
    fn index_mut(&mut self, pid: PeerId) -> &mut Peer<A> {
        self.get_mut(pid).unwrap_or_else(|| panic!("invalid pid"))
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum ChunkOrEvent<'a, A: Address> {
    Chunk(Chunk<'a, A>),
    Connect(PeerId),
    Disconnect(PeerId, &'a [u8]),
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum ChunkType {
    Connless,
    Connected,
    Vital,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Chunk<'a, A: Address> {
    pub data: &'a [u8],
    pub addr: ChunkAddress<A>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum ChunkAddress<A: Address> {
    NonPeerConnless(A),
    Peer(PeerId, ChunkType),
}

struct ConnlessBuilder {
    buffer: [u8; protocol::MAX_PACKETSIZE],
}

impl ConnlessBuilder {
    fn new() -> ConnlessBuilder {
        ConnlessBuilder {
            buffer: [0; protocol::MAX_PACKETSIZE],
        }
    }
    fn send<A: Address, CB: Callback<A>>(&mut self, cb: &mut CB, addr: A, packet: Packet)
        -> Result<(), Error<CB::Error>>
    {
        let send_data = match packet.write(&mut [0u8; 0][..], &mut self.buffer[..]) {
            Ok(d) => d,
            Err(protocol::Error::Capacity(_)) => unreachable!("too short buffer provided"),
            Err(protocol::Error::TooLongData) => return Err(Error::TooLongData),
        };
        try!(cb.send(addr, send_data));
        Ok(())
    }
}

#[derive(Clone)]
pub struct ReceivePacket<'a, A: Address> {
    type_: ReceivePacketType<'a, A>,
}

impl<'a, A: Address> Iterator for ReceivePacket<'a, A> {
    type Item = ChunkOrEvent<'a, A>;
    fn next(&mut self) -> Option<ChunkOrEvent<'a, A>> {
        use self::ReceivePacketType::Connect;
        use self::ReceivePacketType::Connected;
        use self::ReceivePacketType::Connless;
        match self.type_ {
            ReceivePacketType::None => None,
            Connect(ref mut once) => once.next().map(|pid| ChunkOrEvent::Connect(pid)),
            Connected(pid, ref mut receive_packet) => receive_packet.next().map(|chunk| {
                match chunk {
                    ReceiveChunk::Connless(d) => ChunkOrEvent::Chunk(Chunk {
                        data: d,
                        addr: ChunkAddress::Peer(pid, ChunkType::Connless),
                    }),
                    ReceiveChunk::Connected(d, vital) => ChunkOrEvent::Chunk(Chunk {
                        data: d,
                        addr: ChunkAddress::Peer(pid, if vital {
                            ChunkType::Vital
                        } else {
                            ChunkType::Connected
                        }),
                    }),
                    ReceiveChunk::Disconnect(r) => ChunkOrEvent::Disconnect(pid, r),
                }
            }),
            Connless(addr, ref mut once) => once.next().map(|data| {
                ChunkOrEvent::Chunk(Chunk {
                    data: data,
                    addr: ChunkAddress::NonPeerConnless(addr),
                })
            }),
        }
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.clone().count();
        (len, Some(len))
    }
}

impl<'a, A: Address> ExactSizeIterator for ReceivePacket<'a, A> { }

impl<'a, A: Address> ReceivePacket<'a, A> {
    fn none() -> ReceivePacket<'a, A> {
        ReceivePacket {
            type_: ReceivePacketType::None,
        }
    }
    fn connect(pid: PeerId) -> ReceivePacket<'a, A> {
        ReceivePacket {
            type_: ReceivePacketType::Connect(iter::once(pid)),
        }
    }
    fn connected(pid: PeerId, receive_packet: connection::ReceivePacket<'a>, net: &mut Net<A>)
        -> ReceivePacket<'a, A>
    {
        for chunk in receive_packet.clone() {
            if let ReceiveChunk::Disconnect(..) = chunk {
                net.peers.remove_peer(pid);
            }
        }
        ReceivePacket {
            type_: ReceivePacketType::Connected(pid, receive_packet),
        }
    }

    fn connless(addr: A, data: &'a [u8]) -> ReceivePacket<'a, A> {
        ReceivePacket {
            type_: ReceivePacketType::Connless(addr, iter::once(data)),
        }
    }
}

#[derive(Clone)]
enum ReceivePacketType<'a, A: Address> {
    None,
    Connect(iter::Once<PeerId>),
    Connected(PeerId, connection::ReceivePacket<'a>),
    Connless(A, iter::Once<&'a [u8]>),
}

pub struct Net<A: Address> {
    peers: Peers<A>,
    builder: ConnlessBuilder,
}

struct ConnectionCallback<'a, A: Address, CB: Callback<A>+'a> {
    cb: &'a mut CB,
    addr: A,
}

fn cc<A: Address, CB: Callback<A>>(cb: &mut CB, addr: A) -> ConnectionCallback<A, CB> {
    ConnectionCallback {
        cb: cb,
        addr: addr,
    }
}

impl<'a, A: Address, CB: Callback<A>> connection::Callback for ConnectionCallback<'a, A, CB> {
    type Error = CB::Error;
    fn send(&mut self, data: &[u8]) -> Result<(), CB::Error> {
        self.cb.send(self.addr, data)
    }
}

impl<A: Address> Net<A> {
    pub fn new() -> Net<A> {
        Net {
            peers: Peers::new(),
            builder: ConnlessBuilder::new(),
        }
    }
    pub fn connect<CB: Callback<A>>(&mut self, cb: &mut CB, addr: A)
        -> (PeerId, Result<(), CB::Error>)
    {
        let (pid, peer) = self.peers.new_peer(addr);
        (pid, peer.conn.connect(&mut cc(cb, peer.addr)))
    }
    pub fn disconnect<CB: Callback<A>>(&mut self, cb: &mut CB, pid: PeerId, reason: &[u8])
        -> Result<(), CB::Error>
    {
        let peer = &mut self.peers[pid];
        peer.conn.disconnect(&mut cc(cb, peer.addr), reason)
    }
    pub fn send_connless<CB: Callback<A>>(&mut self, cb: &mut CB, addr: A, data: &[u8])
        -> Result<(), Error<CB::Error>>
    {
        self.builder.send(cb, addr, Packet::Connless(data))
    }
    pub fn send<CB: Callback<A>>(&mut self, cb: &mut CB, chunk: Chunk<A>)
        -> Result<(), Error<CB::Error>>
    {
        match chunk.addr {
            ChunkAddress::NonPeerConnless(a) => {
                if let Some(pid) = self.peers.pid_from_addr(a) {
                    return self.peers[pid].conn.send_connless(&mut cc(cb, a), chunk.data)
                }
                self.send_connless(cb, a, chunk.data)
            }
            ChunkAddress::Peer(pid, type_) => {
                let peer = &mut self.peers[pid];
                let vital = match type_ {
                    ChunkType::Connless => 
                        return peer.conn.send_connless(&mut cc(cb, peer.addr), chunk.data),
                    ChunkType::Connected => false,
                    ChunkType::Vital => true,
                };
                peer.conn.send(&mut cc(cb, peer.addr), chunk.data, vital)
            }
        }
    }
    pub fn peer_addr(&self, pid: PeerId) -> Option<A> {
        self.peers.get(pid).map(|p| p.addr)
    }
    pub fn feed<'a, CB: Callback<A>, B: Buffer<'a>>(&mut self, cb: &mut CB, addr: A, data: &'a [u8], buf: B)
        -> (ReceivePacket<'a, A>, Result<(), CB::Error>)
    {
        with_buffer(buf, |b| self.feed_impl(cb, addr, data, b))
    }

    fn feed_impl<'d, 's, CB: Callback<A>>(&mut self, cb: &mut CB, addr: A, data: &'d [u8], mut buf: BufferRef<'d, 's>)
        -> (ReceivePacket<'d, A>, Result<(), CB::Error>)
    {
        if let Some(pid) = self.peers.pid_from_addr(addr) {
            let (packet, e) = self.peers[pid].conn.feed(&mut cc(cb, addr), data, &mut buf);
            (ReceivePacket::connected(pid, packet, self), e)
        } else {
            let packet = Packet::read(data, &mut buf);
            if let Some(Packet::Connless(d)) = packet {
                (ReceivePacket::connless(addr, d), Ok(()))
            } else if let Some(Packet::Connected(ConnectedPacket {
                    type_: ConnectedPacketType::Control(ControlPacket::Connect), ..
                })) = packet
            {
                let (pid, peer) = self.peers.new_peer(addr);
                let (mut none, e) = peer.conn.feed(&mut cc(cb, peer.addr), data, &mut buf);
                assert!(none.next().is_none());
                (ReceivePacket::connect(pid), e)
            } else {
                // WARN
                (ReceivePacket::none(), Ok(()))
            }
        }
    }
}

#[cfg(test)]
mod test {
    use itertools::Itertools;
    use protocol;
    use std::collections::VecDeque;
    use super::Callback;
    use super::ChunkOrEvent;
    use super::Net;
    use void::ResultVoidExt;
    use void::Void;

    #[test]
    fn establish_connection() {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        enum Address {
            Client,
            Server,
        }
        struct Cb {
            packets: VecDeque<Vec<u8>>,
            recipient: Address,
        }
        impl Cb {
            fn new() -> Cb {
                Cb {
                    packets: VecDeque::new(),
                    recipient: Address::Server,
                }
            }
        }
        impl Callback<Address> for Cb {
            type Error = Void;
            fn send(&mut self, addr: Address, data: &[u8]) -> Result<(), Void> {
                assert!(self.recipient == addr);
                self.packets.push_back(data.to_owned());
                Ok(())
            }
        }
        let mut cb = Cb::new();
        let cb = &mut cb;
        let mut buffer = [0; protocol::MAX_PAYLOAD];

        let mut net = Net::new();

        // Connect
        cb.recipient = Address::Server;
        let (c_pid, res) = net.connect(cb, Address::Server);
        res.void_unwrap();
        let packet = cb.packets.pop_front().unwrap();
        assert!(cb.packets.is_empty());

        // ConnectAccept
        cb.recipient = Address::Client;
        let s_pid;
        {
            let p = net.feed(cb, Address::Client, &packet, &mut buffer[..]).0.collect_vec();
            assert!(p.len() == 1);
            if let ChunkOrEvent::Connect(s) = p[0] {
                s_pid = s;
            } else {
                panic!();
            }
        }
        let packet = cb.packets.pop_front().unwrap();
        assert!(cb.packets.is_empty());

        // Accept
        cb.recipient = Address::Server;
        assert!(net.feed(cb, Address::Server, &packet, &mut buffer[..]).0.next().is_none());
        let packet = cb.packets.pop_front().unwrap();
        assert!(cb.packets.is_empty());

        cb.recipient = Address::Client;
        assert!(net.feed(cb, Address::Client, &packet, &mut buffer[..]).0.next().is_none());
        assert!(cb.packets.is_empty());

        // Disconnect
        cb.recipient = Address::Server;
        net.disconnect(cb, c_pid, b"foobar").void_unwrap();
        let packet = cb.packets.pop_front().unwrap();
        assert!(cb.packets.is_empty());

        cb.recipient = Address::Client;
        assert!(net.feed(cb, Address::Client, &packet, &mut buffer[..]).0.collect_vec()
                == &[ChunkOrEvent::Disconnect(s_pid, b"foobar")]);
        assert!(cb.packets.is_empty());
    }
}

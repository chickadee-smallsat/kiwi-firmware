//! A lightweight peer-to-peer datagram protocol for up to [`MAX_PEERS`] (12)
//! nodes sharing one CAN bus, carrying messages of up to
//! [`MAX_P2P_MSG_LEN`] (256) bytes each.
//!
//! Unlike [`crate::IsoTpChannel`] (a manually-addressed, flow-controlled,
//! strictly point-to-point ISO-TP session), this protocol bakes addressing
//! directly into a 29-bit extended CAN identifier, so every peer can address
//! every other peer (or all of them at once) without agreeing on a dedicated
//! CAN ID pair up front:
//!
//! ```text
//! ID (29 bits): C B T A r SSSSSSSSSSSS RRRRRRRRRRRR
//!               | | | | |   \___ sender __/\___ receiver __/
//!               | | | | \_ reserved, always 0
//!               | | | \_ arbitration flag (private — see `crate::busarb`)
//!               | | \_ timesync flag (private — see below)
//!               | \_ broadcast flag
//!               \_ control flag (see `P2pChannel`)
//! ```
//!
//! `sender` and `receiver` are **one-hot**: peer `n`'s address is bit `n` of
//! whichever field names it, not the 4-bit numeric value `n` itself. A
//! sender always sets exactly one bit in `sender`; a unicast frame sets
//! exactly one bit in `receiver` and leaves the broadcast flag clear, while
//! a broadcast frame sets the broadcast flag and leaves `receiver` entirely
//! `0` (so [`Destination::Broadcast`] doesn't need to consume, or duplicate
//! into, any peer's own bit). Any frame that doesn't fit this shape — more
//! than one sender bit, a nonzero receiver field alongside the broadcast
//! flag, a nonzero reserved field, more than one of the control/timesync/
//! arbitration flags set, or a standard (11-bit) ID — cannot be one of ours
//! and is silently ignored.
//!
//! One-hot addressing over a 4-bit numeric field costs 8 extra ID bits to
//! address 12 peers instead of 16, but in return a peer's own hardware
//! acceptance filter reduces to "is my bit set" — a single fixed bit
//! position, known as soon as a [`PeerAddr`] is chosen, rather than a
//! multi-bit value that has to be compared for equality against a runtime
//! field. See [`hardware_filter`].
//!
//! [`P2pChannel`] selects which of two *application-visible* kinds a message
//! belongs to, via the control flag bit: [`P2pChannel::App`] (the default,
//! used by [`P2pSender::send`] and [`P2pSender::broadcast`]) can carry up to
//! [`MAX_P2P_MSG_LEN`] bytes, while [`P2pChannel::Control`] is free for the
//! application's own control-style protocols and carries a much smaller
//! [`MAX_CONTROL_MSG_LEN`] — such protocols are expected to only ever
//! exchange small fixed-size messages, so [`P2pReceiver`] doesn't reserve a
//! full [`MAX_P2P_MSG_LEN`]-byte reassembly buffer per peer for them. This is
//! a CAN ID-level distinction, not anything either channel's *payload*
//! contains: an application's own message bytes can never be misread as a
//! control-channel message (or vice versa) no matter what they contain, and
//! each channel gets independent reassembly state per source peer, so an
//! interleaved control message can't corrupt an in-progress application
//! message from the same peer either.
//!
//! The timesync flag marks a third kind, reserved exclusively for
//! [`crate::timesync`]'s own wire traffic, with its own independent
//! reassembly state again — but unlike `App`/`Control` it has **no**
//! [`P2pChannel`] variant and never appears anywhere in this module's public
//! API: [`P2pReceiver::receive`] silently drops it rather than surfacing it,
//! the same way it drops any other frame that isn't meant for the caller.
//! The only way to observe or produce it is through
//! [`crate::TimeSyncMaster`]/[`crate::TimeSyncFollower`]/[`crate::ProtocolRunner`].
//! This closes off the same conflict `Control` vs. `App` already avoids, one
//! level up: without a dedicated, *unreachable* channel of its own, a second
//! control-style protocol built by an application on [`P2pChannel::Control`]
//! would collide with time sync's reassembly state exactly the way `App` and
//! `Control` used to collide with each other before that split existed.
//!
//! The arbitration flag marks a fourth kind, reserved the same way for
//! [`crate::busarb`]'s own wire traffic (peer membership, role registration,
//! and bus-access coordination) — likewise with no [`P2pChannel`] variant
//! and no way to observe or produce it except through that module.
//!
//! Because ID derivation is entirely deterministic, one [`P2pReceiver`] can
//! be the sole consumer of the underlying [`Mcp25625Handle`]'s incoming
//! queue and demultiplex traffic for *all* peers *and* every channel itself
//! — this is the piece [`crate::IsoTpChannel`]'s docs call out as
//! unsupported for concurrent multi-peer sessions sharing one handle (each
//! channel would otherwise compete for the same frames). [`new_p2p`] hands
//! back a cheaply-clonable [`P2pSender`] (for any number of application
//! tasks to originate messages from) and a single [`P2pReceiver`] (meant to
//! be polled by one task in a loop, dispatching whatever
//! `(PeerAddr, P2pChannel, message)` triples it returns onward as the
//! application sees fit).
//!
//! There is no flow control, retransmission, or acknowledgement above what
//! the CAN data-link layer itself already provides (CRC, ACK slot, automatic
//! retransmission up to the error limits) — this is a best-effort, unordered
//! *between* peers (though in-order *within* one peer's own stream, thanks to
//! CAN's own bus arbitration and each frame's monotonic sequence number)
//! datagram service, not a reliable byte stream. A malformed or out-of-order
//! frame simply causes that peer's in-flight message to be dropped and
//! reassembly to restart from its next First Frame.
//!
//! This protocol claims every extended CAN ID of the shape above; sharing
//! the bus with other traffic that happens to fall into that pattern
//! (reserved bits clear, exactly one sender bit, and either the broadcast
//! flag or exactly one receiver bit) would be misinterpreted as
//! peer-to-peer frames.

use heapless::Vec;

use crate::{
    config::RxFilter,
    frame::{CanFrame, ExtendedId, Id, MAX_DATA_LEN},
    runner::Mcp25625Handle,
};
use embassy_sync::blocking_mutex::raw::RawMutex;

/// Number of addressable peers — one-hot in a 12-bit field, so `0..=11`
/// (see the module docs for why this isn't a 4-bit numeric field instead).
pub const MAX_PEERS: usize = 12;
/// Largest message payload the [`P2pChannel::App`] channel can carry in one
/// send/receive call.
pub const MAX_P2P_MSG_LEN: usize = 256;
/// Largest message payload the [`P2pChannel::Control`] channel can carry in
/// one send/receive call. Much smaller than [`MAX_P2P_MSG_LEN`], since
/// control-style protocols only ever need small, fixed-size messages — this
/// keeps [`P2pReceiver`]'s per-peer Control reassembly buffer cheap even
/// though one is reserved for every peer whether or not it's ever used.
pub const MAX_CONTROL_MSG_LEN: usize = 16;
/// Largest message payload the private timesync wire channel can carry —
/// sized exactly to [`crate::timesync`]'s own largest message. Not public:
/// this channel is never reachable through this module's API (see the
/// module docs), so there's no reason to give it the kind of external growth
/// headroom [`MAX_CONTROL_MSG_LEN`] has.
pub(crate) const MAX_TIMESYNC_MSG_LEN: usize = 10;
/// Largest message payload the private bus-arbitration wire channel can
/// carry — sized to [`crate::busarb`]'s largest message (a full peer-list
/// response). Not public, for the same reason as [`MAX_TIMESYNC_MSG_LEN`].
pub(crate) const MAX_ARBITRATION_MSG_LEN: usize = 80;

/// Bit 28: set for [`P2pChannel::Control`] frames, clear otherwise.
const CONTROL_BIT: u32 = 1 << 28;
/// Bit 27: set to mark a frame as addressed to every peer.
const BROADCAST_BIT: u32 = 1 << 27;
/// Bit 26: set for the private timesync channel's frames (see the module
/// docs), clear otherwise. Mutually exclusive with [`CONTROL_BIT`] and
/// [`ARBITRATION_BIT`]; a frame with more than one of the three set is
/// malformed.
const TIMESYNC_BIT: u32 = 1 << 26;
/// Bit 25: set for the private bus-arbitration channel's frames (see
/// [`crate::busarb`] and the module docs), clear otherwise. Mutually
/// exclusive with [`CONTROL_BIT`] and [`TIMESYNC_BIT`].
const ARBITRATION_BIT: u32 = 1 << 25;
/// Bit 24: always clear. Checked on receive so that non-p2p traffic sharing
/// bits 23:0 by coincidence isn't misread as a p2p frame just because it
/// also happens to have a single sender/receiver bit set.
const RESERVED_MASK: u32 = 1 << 24;
/// Where the one-hot sender field starts (bits 23:12).
const SENDER_SHIFT: u32 = 12;
/// The one-hot sender field (bits 23:12).
const SENDER_MASK: u32 = 0xFFF << SENDER_SHIFT;
/// The one-hot receiver field (bits 11:0); all-zero for a broadcast frame.
const RECEIVER_MASK: u32 = 0xFFF;

/// A peer address: one of [`MAX_PEERS`] (12) fixed slots, each occupying one
/// bit position in the one-hot sender/receiver fields (see the module docs)
/// — `PeerN` is bit `N`. A closed enum rather than a wrapped integer, since
/// there are only ever exactly `MAX_PEERS` valid addresses to begin with.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum PeerAddr {
    /// Bit position 0.
    Peer0 = 0,
    /// Bit position 1.
    Peer1 = 1,
    /// Bit position 2.
    Peer2 = 2,
    /// Bit position 3.
    Peer3 = 3,
    /// Bit position 4.
    Peer4 = 4,
    /// Bit position 5.
    Peer5 = 5,
    /// Bit position 6.
    Peer6 = 6,
    /// Bit position 7.
    Peer7 = 7,
    /// Bit position 8.
    Peer8 = 8,
    /// Bit position 9.
    Peer9 = 9,
    /// Bit position 10.
    Peer10 = 10,
    /// Bit position 11.
    Peer11 = 11,
}

// If MAX_PEERS ever changes, PeerAddr's variant list, `new`, and `as_u8`
// below all need updating to match — this keeps that drift from silently
// compiling.
const _: () = assert!(MAX_PEERS == 12, "PeerAddr's variants must match MAX_PEERS");

impl PeerAddr {
    /// Creates a `PeerAddr` from a raw bit position, returning `None` if
    /// `addr` is not in `0..MAX_PEERS`.
    pub const fn new(addr: u8) -> Option<Self> {
        match addr {
            0 => Some(Self::Peer0),
            1 => Some(Self::Peer1),
            2 => Some(Self::Peer2),
            3 => Some(Self::Peer3),
            4 => Some(Self::Peer4),
            5 => Some(Self::Peer5),
            6 => Some(Self::Peer6),
            7 => Some(Self::Peer7),
            8 => Some(Self::Peer8),
            9 => Some(Self::Peer9),
            10 => Some(Self::Peer10),
            11 => Some(Self::Peer11),
            _ => None,
        }
    }

    /// Returns the raw bit position (`0..MAX_PEERS`).
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Destination of a peer-to-peer send: either one specific peer, or every
/// peer on the bus at once (see the module docs for how this is encoded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Destination {
    /// A single addressed peer.
    Peer(PeerAddr),
    /// Every peer on the bus.
    Broadcast,
}

impl From<PeerAddr> for Destination {
    fn from(addr: PeerAddr) -> Self {
        Destination::Peer(addr)
    }
}

/// Errors from a [`P2pSender::send`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum P2pError {
    /// The payload is larger than the destination channel's own limit.
    TooLarge,
}

/// Which application-visible channel a p2p message travels on — see the
/// module docs for why this exists and how it's encoded, and for the third,
/// entirely private channel `crate::timesync` uses instead of either of
/// these (which is why it has no variant here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum P2pChannel {
    /// Ordinary application messages.
    App,
    /// The application's own control-style protocols — never interpreted,
    /// generated, or otherwise touched by this crate.
    Control,
}

/// Wire-level counterpart to [`P2pChannel`], with two further values
/// reserved exclusively for `crate::timesync`'s and `crate::busarb`'s own
/// traffic. Never exposed outside this crate — see [`P2pChannel`] and the
/// module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WireChannel {
    App,
    Control,
    TimeSync,
    Arbitration,
}

impl From<P2pChannel> for WireChannel {
    fn from(channel: P2pChannel) -> Self {
        match channel {
            P2pChannel::App => WireChannel::App,
            P2pChannel::Control => WireChannel::Control,
        }
    }
}

/// Computes the CAN ID for a frame from `dest` to `src` on `channel`: one
/// bit set in the sender field at `src`'s bit position; for
/// [`Destination::Peer`], one bit set in the receiver field at that peer's
/// bit position and the broadcast flag clear; for [`Destination::Broadcast`],
/// the receiver field left all-zero and the broadcast flag set. See the
/// module docs for the full layout.
fn frame_id(dest: Destination, src: PeerAddr, channel: WireChannel) -> Id {
    let (control_bit, timesync_bit, arbitration_bit) = match channel {
        WireChannel::App => (0, 0, 0),
        WireChannel::Control => (CONTROL_BIT, 0, 0),
        WireChannel::TimeSync => (0, TIMESYNC_BIT, 0),
        WireChannel::Arbitration => (0, 0, ARBITRATION_BIT),
    };
    let (receiver_field, broadcast_bit) = match dest {
        Destination::Peer(addr) => (1u32 << addr.as_u8(), 0),
        Destination::Broadcast => (0, BROADCAST_BIT),
    };
    let sender_field = 1u32 << src.as_u8();
    let raw = control_bit
        | timesync_bit
        | arbitration_bit
        | broadcast_bit
        | (sender_field << SENDER_SHIFT)
        | receiver_field;
    Id::Extended(ExtendedId::new(raw).expect(
        "control/broadcast/timesync/arbitration/sender/receiver fields fit within 29 bits by construction",
    ))
}

/// Recovers `(dest, src, channel)` from a CAN ID built by [`frame_id`], or
/// `None` if `id` doesn't have that shape (see the module docs) and so
/// cannot be one of ours.
fn parse_frame_id(id: Id) -> Option<(Destination, PeerAddr, WireChannel)> {
    let Id::Extended(eid) = id else {
        return None;
    };
    let raw = eid.as_raw();
    if raw & RESERVED_MASK != 0 {
        return None;
    }

    let channel = match (
        raw & CONTROL_BIT != 0,
        raw & TIMESYNC_BIT != 0,
        raw & ARBITRATION_BIT != 0,
    ) {
        (false, false, false) => WireChannel::App,
        (true, false, false) => WireChannel::Control,
        (false, true, false) => WireChannel::TimeSync,
        (false, false, true) => WireChannel::Arbitration,
        _ => return None,
    };

    let sender_field = (raw & SENDER_MASK) >> SENDER_SHIFT;
    if sender_field.count_ones() != 1 {
        return None;
    }
    let src = PeerAddr::new(sender_field.trailing_zeros() as u8)?;

    let dest = if raw & BROADCAST_BIT != 0 {
        if raw & RECEIVER_MASK != 0 {
            return None;
        }
        Destination::Broadcast
    } else {
        let receiver_field = raw & RECEIVER_MASK;
        if receiver_field.count_ones() != 1 {
            return None;
        }
        Destination::Peer(PeerAddr::new(receiver_field.trailing_zeros() as u8)?)
    };

    Some((dest, src, channel))
}

/// Per-frame control byte: bit 7 is the MORE flag (more frames follow this
/// one), bits 6:0 are a sequence number. Sequence `0` always marks a First
/// Frame, which additionally carries a 2-byte total length right after the
/// control byte; every other sequence number is a plain continuation
/// carrying up to 7 payload bytes with no extra header.
const MORE_FLAG: u8 = 0x80;
const SEQ_MASK: u8 = 0x7F;
/// Payload bytes available in a First Frame (1 control + 2 length bytes leave 5).
const FIRST_FRAME_CHUNK: usize = MAX_DATA_LEN - 3;
/// Payload bytes available in a continuation frame (1 control byte leaves 7).
const CONT_FRAME_CHUNK: usize = MAX_DATA_LEN - 1;

/// Cheaply clonable sender half of a peer-to-peer session; see the module
/// docs and [`new_p2p`].
pub struct P2pSender<'ch, M, const TXN: usize, const RXN: usize>
where
    M: RawMutex,
{
    can: Mcp25625Handle<'ch, M, TXN, RXN>,
    local: PeerAddr,
}

// Written by hand rather than `#[derive(Clone)]`: the derive macro adds an
// `M: Clone` bound it can't see past, even though `Mcp25625Handle` itself
// (see `runner.rs`) is cheaply `Clone` for any `M: RawMutex` regardless —
// most `RawMutex` implementors, including the very common `NoopRawMutex`,
// aren't `Clone` themselves, so the derived bound would make `clone()`
// uncallable for exactly the mutex type this workspace's own firmware uses.
impl<M, const TXN: usize, const RXN: usize> Clone for P2pSender<'_, M, TXN, RXN>
where
    M: RawMutex,
{
    fn clone(&self) -> Self {
        Self {
            can: self.can.clone(),
            local: self.local,
        }
    }
}

impl<'ch, M, const TXN: usize, const RXN: usize> P2pSender<'ch, M, TXN, RXN>
where
    M: RawMutex,
{
    /// Sends `data` to `dest` — either a single [`PeerAddr`] or
    /// [`Destination::Broadcast`] — as this peer's own address, on the
    /// [`P2pChannel::App`] channel. Equivalent to
    /// `send_on(P2pChannel::App, dest, data)`; see that method for details.
    pub async fn send(&self, dest: impl Into<Destination>, data: &[u8]) -> Result<(), P2pError> {
        self.send_on(P2pChannel::App, dest, data).await
    }

    /// Sends `data` to every peer on the bus at once, on the
    /// [`P2pChannel::App`] channel. Equivalent to
    /// `send_on(P2pChannel::App, Destination::Broadcast, data)`.
    pub async fn broadcast(&self, data: &[u8]) -> Result<(), P2pError> {
        self.send_on(P2pChannel::App, Destination::Broadcast, data)
            .await
    }

    /// Sends `data` to every peer on the bus at once, on `channel`.
    /// Equivalent to `send_on(channel, Destination::Broadcast, data)`.
    pub async fn broadcast_on(&self, channel: P2pChannel, data: &[u8]) -> Result<(), P2pError> {
        self.send_on(channel, Destination::Broadcast, data).await
    }

    /// Sends `data` to `dest` on `channel`, splitting it across as many
    /// frames as needed (see the module docs for the wire format and for
    /// what a channel is). Returns once every fragment has been handed to
    /// the underlying [`Mcp25625Handle`]'s TX queue; this is not a delivery
    /// guarantee — there is no flow control or acknowledgement in this
    /// protocol.
    ///
    /// Returns [`P2pError::TooLarge`] if `data` exceeds `channel`'s own
    /// limit — [`MAX_P2P_MSG_LEN`] for [`P2pChannel::App`], the much
    /// smaller [`MAX_CONTROL_MSG_LEN`] for [`P2pChannel::Control`] — even
    /// though both fit within a single CAN frame's worth of headroom here;
    /// the receiving [`P2pReceiver`] only reserves a reassembly buffer sized
    /// for `channel`'s own limit and would otherwise have to silently drop
    /// an oversized Control message it has no room to hold.
    pub async fn send_on(
        &self,
        channel: P2pChannel,
        dest: impl Into<Destination>,
        data: &[u8],
    ) -> Result<(), P2pError> {
        let limit = match channel {
            P2pChannel::App => MAX_P2P_MSG_LEN,
            P2pChannel::Control => MAX_CONTROL_MSG_LEN,
        };
        self.send_raw(channel.into(), limit, dest, data).await
    }

    /// Crate-internal counterpart to [`Self::send_on`] for the private
    /// timesync wire channel, which has no [`P2pChannel`] value to name it
    /// with — used solely by `crate::timesync`.
    pub(crate) async fn send_timesync(
        &self,
        dest: impl Into<Destination>,
        data: &[u8],
    ) -> Result<(), P2pError> {
        self.send_raw(WireChannel::TimeSync, MAX_TIMESYNC_MSG_LEN, dest, data)
            .await
    }

    /// Crate-internal counterpart to [`Self::broadcast_on`] for the private
    /// timesync wire channel — used solely by `crate::timesync`.
    pub(crate) async fn broadcast_timesync(&self, data: &[u8]) -> Result<(), P2pError> {
        self.send_timesync(Destination::Broadcast, data).await
    }

    /// Crate-internal counterpart to [`Self::send_on`] for the private
    /// bus-arbitration wire channel, which has no [`P2pChannel`] value to
    /// name it with — used solely by `crate::busarb`.
    pub(crate) async fn send_arbitration(
        &self,
        dest: impl Into<Destination>,
        data: &[u8],
    ) -> Result<(), P2pError> {
        self.send_raw(
            WireChannel::Arbitration,
            MAX_ARBITRATION_MSG_LEN,
            dest,
            data,
        )
        .await
    }

    /// Crate-internal counterpart to [`Self::broadcast_on`] for the private
    /// bus-arbitration wire channel — used solely by `crate::busarb`.
    pub(crate) async fn broadcast_arbitration(&self, data: &[u8]) -> Result<(), P2pError> {
        self.send_arbitration(Destination::Broadcast, data).await
    }

    async fn send_raw(
        &self,
        channel: WireChannel,
        limit: usize,
        dest: impl Into<Destination>,
        data: &[u8],
    ) -> Result<(), P2pError> {
        if data.len() > limit {
            return Err(P2pError::TooLarge);
        }
        let id = frame_id(dest.into(), self.local, channel);
        let mut sent = 0usize;
        let mut seq: u8 = 0;
        loop {
            let mut buf = [0u8; MAX_DATA_LEN];
            let header_len = if seq == 0 {
                let len = data.len() as u16;
                buf[1] = (len >> 8) as u8;
                buf[2] = (len & 0xFF) as u8;
                3
            } else {
                1
            };
            let chunk_cap = if seq == 0 {
                FIRST_FRAME_CHUNK
            } else {
                CONT_FRAME_CHUNK
            };
            let chunk = (data.len() - sent).min(chunk_cap);
            buf[header_len..header_len + chunk].copy_from_slice(&data[sent..sent + chunk]);
            sent += chunk;
            let more = sent < data.len();
            buf[0] = (if more { MORE_FLAG } else { 0 }) | (seq & SEQ_MASK);

            let frame = CanFrame::new(id, &buf[..header_len + chunk])
                .expect("p2p frames are always built within the 8-byte classic CAN payload");
            self.can.transmit_impl(frame).await;

            if !more {
                return Ok(());
            }
            // MAX_P2P_MSG_LEN (256) needs at most 1 + ((256 - 5) / 7).ceil() =
            // 37 frames, well under the 7-bit sequence number's range of 128.
            seq = seq.wrapping_add(1);
        }
    }
}

/// Per-source reassembly state held by a [`P2pReceiver`], generic over its
/// channel's own message size limit `N` (see [`MAX_P2P_MSG_LEN`],
/// [`MAX_CONTROL_MSG_LEN`], and [`MAX_TIMESYNC_MSG_LEN`]).
struct PeerReassembly<const N: usize> {
    buf: Vec<u8, N>,
    declared_len: usize,
    next_seq: u8,
    active: bool,
}

impl<const N: usize> PeerReassembly<N> {
    const fn new() -> Self {
        Self {
            buf: Vec::new(),
            declared_len: 0,
            next_seq: 0,
            active: false,
        }
    }

    /// Abandons whatever partial message this peer had in flight.
    fn reset(&mut self) {
        self.active = false;
        self.buf.clear();
    }

    /// Feeds one non-empty p2p frame payload into this slot. Returns
    /// `Some(payload)` once a message completes successfully; `None`
    /// otherwise, whether still in progress or just dropped due to a
    /// malformed/out-of-sequence frame (the caller doesn't need to tell
    /// those apart, since it always just goes back to waiting either way —
    /// see the module docs on this being a best-effort datagram service).
    fn feed(&mut self, data: &[u8]) -> Option<Vec<u8, N>> {
        let control = data[0];
        let more = control & MORE_FLAG != 0;
        let seq = control & SEQ_MASK;

        if seq == 0 {
            if data.len() < 3 {
                self.reset();
                return None;
            }
            let len = ((data[1] as usize) << 8) | data[2] as usize;
            if len > N {
                self.reset();
                return None;
            }
            self.buf.clear();
            // `data.len() - 3 <= FIRST_FRAME_CHUNK` always holds since the
            // frame came from a real CAN buffer of at most 8 bytes.
            let _ = self.buf.extend_from_slice(&data[3..]);
            self.declared_len = len;
            self.next_seq = 1;
            self.active = true;
        } else {
            if !self.active || seq != self.next_seq {
                self.reset();
                return None;
            }
            if self.buf.extend_from_slice(&data[1..]).is_err() {
                // Would overflow N; the sender's declared length must have
                // been wrong (or corrupted). Drop it.
                self.reset();
                return None;
            }
            self.next_seq = self.next_seq.wrapping_add(1);
        }

        if !more {
            let complete = self.buf.len() == self.declared_len;
            self.active = false;
            if complete {
                return Some(core::mem::take(&mut self.buf));
            }
            // Length mismatch: drop the message and keep waiting.
            self.buf.clear();
        }
        None
    }
}

/// Receiver half of a peer-to-peer session, demultiplexing frames from up to
/// [`MAX_PEERS`] sources — and, independently, every channel — at once; see
/// the module docs and [`new_p2p`].
///
/// Meant to be owned and polled by a single task: like the raw
/// [`crate::Mcp25625`] driver, it holds `&mut self` reassembly state per
/// source peer, so it is not meant to be split further across tasks (use
/// [`P2pSender`], which is `Clone`, for concurrent senders instead).
pub struct P2pReceiver<'ch, M, const TXN: usize, const RXN: usize>
where
    M: RawMutex,
{
    can: Mcp25625Handle<'ch, M, TXN, RXN>,
    local: PeerAddr,
    promiscuous: bool,
    /// [`P2pChannel::App`] reassembly state, one slot per peer.
    app: [PeerReassembly<MAX_P2P_MSG_LEN>; MAX_PEERS],
    /// [`P2pChannel::Control`] reassembly state, one slot per peer — kept
    /// entirely separate from `app` and `timesync` (rather than one array
    /// shared by all channels) both so it can be sized differently, and so
    /// an interleaved message on one channel can never corrupt an
    /// in-progress message on another from the same peer.
    control: [PeerReassembly<MAX_CONTROL_MSG_LEN>; MAX_PEERS],
    /// Private timesync-channel reassembly state, one slot per peer — see
    /// the module docs. Only ever populated or drained via
    /// [`Self::receive_raw`], never [`Self::receive`].
    timesync: [PeerReassembly<MAX_TIMESYNC_MSG_LEN>; MAX_PEERS],
    /// Private bus-arbitration-channel reassembly state, one slot per peer —
    /// see the module docs and [`crate::busarb`]. Only ever populated or
    /// drained via [`Self::receive_raw`], never [`Self::receive`].
    arbitration: [PeerReassembly<MAX_ARBITRATION_MSG_LEN>; MAX_PEERS],
}

impl<'ch, M, const TXN: usize, const RXN: usize> P2pReceiver<'ch, M, TXN, RXN>
where
    M: RawMutex,
{
    /// Enables or disables promiscuous mode: when `true`, [`Self::receive`]
    /// returns every successfully reassembled message regardless of who
    /// it's addressed to, instead of only ones addressed to `local` or
    /// broadcast. Off by default.
    ///
    /// This only changes the *software* destination check — it has no
    /// effect on whatever `RxFilter` the device itself was initialized
    /// with. If the driver was configured with [`hardware_filter`] (or any
    /// other `RxFilter` that excludes some identifiers), the MCP25625 will
    /// still have discarded non-matching frames before they ever reach this
    /// receiver, so promiscuous mode alone won't see them; pair it with
    /// [`crate::RxFilter::AcceptAll`] (the default) to actually observe
    /// every frame on the bus.
    pub fn set_promiscuous(&mut self, promiscuous: bool) {
        self.promiscuous = promiscuous;
    }

    /// Waits for and reassembles the next complete [`P2pChannel::App`] or
    /// [`P2pChannel::Control`] message addressed to this receiver's local
    /// peer address (unicast) or to every peer (broadcast) — or, in
    /// promiscuous mode (see [`Self::set_promiscuous`]), the next complete
    /// message from *any* peer regardless of who it's addressed to —
    /// returning its sender, its channel, and its payload.
    ///
    /// Frames addressed to a different peer (outside promiscuous mode),
    /// frames from a peer with no reassembly in progress that aren't
    /// themselves a First Frame, out-of-sequence continuations, and any
    /// frame that doesn't parse as a valid p2p frame at all (see the module
    /// docs) are all silently dropped (resetting that source's in-flight
    /// message, if any) rather than surfaced as errors — see the module
    /// docs on this being a best-effort datagram service.
    ///
    /// This performs the destination check in software regardless of
    /// whatever hardware filtering is configured on the underlying device —
    /// see [`hardware_filter`] to additionally have the MCP25625 itself
    /// discard frames addressed to other peers before an interrupt is even
    /// raised.
    pub async fn receive(&mut self) -> (PeerAddr, P2pChannel, Vec<u8, MAX_P2P_MSG_LEN>) {
        // The private timesync channel (see the module docs) never appears
        // here: a completed timesync message is silently discarded just like
        // any other frame this receiver isn't interested in. Use
        // [`crate::ProtocolRunner`] to actually participate in time sync,
        // which reads that channel through [`Self::receive_raw`] instead.
        loop {
            let (src, channel, payload) = self.receive_raw().await;
            match channel {
                WireChannel::App => return (src, P2pChannel::App, payload),
                WireChannel::Control => return (src, P2pChannel::Control, payload),
                WireChannel::TimeSync | WireChannel::Arbitration => continue,
            }
        }
    }

    /// Crate-internal counterpart to [`Self::receive`] that additionally
    /// surfaces the private timesync and bus-arbitration channels — used
    /// solely by [`crate::ProtocolRunner`], the only thing allowed to
    /// observe them (see the module docs).
    pub(crate) async fn receive_raw(
        &mut self,
    ) -> (PeerAddr, WireChannel, Vec<u8, MAX_P2P_MSG_LEN>) {
        loop {
            let frame = self.can.receive_impl().await;
            let Some((dest, src, channel)) = parse_frame_id(frame.id()) else {
                continue;
            };
            let deliver_to_us = self.promiscuous
                || match dest {
                    Destination::Broadcast => true,
                    Destination::Peer(addr) => addr == self.local,
                };
            if !deliver_to_us {
                continue;
            }
            let data = frame.data();
            if data.is_empty() {
                continue;
            }

            match channel {
                WireChannel::App => {
                    let slot = &mut self.app[src.as_u8() as usize];
                    if let Some(payload) = slot.feed(data) {
                        return (src, WireChannel::App, payload);
                    }
                }
                WireChannel::Control => {
                    let slot = &mut self.control[src.as_u8() as usize];
                    if let Some(payload) = slot.feed(data) {
                        let payload = Vec::from_slice(&payload)
                            .expect("MAX_CONTROL_MSG_LEN <= MAX_P2P_MSG_LEN, so this always fits");
                        return (src, WireChannel::Control, payload);
                    }
                }
                WireChannel::TimeSync => {
                    let slot = &mut self.timesync[src.as_u8() as usize];
                    if let Some(payload) = slot.feed(data) {
                        let payload = Vec::from_slice(&payload)
                            .expect("MAX_TIMESYNC_MSG_LEN <= MAX_P2P_MSG_LEN, so this always fits");
                        return (src, WireChannel::TimeSync, payload);
                    }
                }
                WireChannel::Arbitration => {
                    let slot = &mut self.arbitration[src.as_u8() as usize];
                    if let Some(payload) = slot.feed(data) {
                        let payload = Vec::from_slice(&payload).expect(
                            "MAX_ARBITRATION_MSG_LEN <= MAX_P2P_MSG_LEN, so this always fits",
                        );
                        return (src, WireChannel::Arbitration, payload);
                    }
                }
            }
        }
    }
}

/// Splits a [`Mcp25625Handle`] into a peer-to-peer [`P2pSender`]/[`P2pReceiver`]
/// pair for `local`'s own address. See the module docs for the addressing
/// scheme and delivery guarantees.
pub fn new_p2p<'ch, M, const TXN: usize, const RXN: usize>(
    can: Mcp25625Handle<'ch, M, TXN, RXN>,
    local: PeerAddr,
) -> (P2pSender<'ch, M, TXN, RXN>, P2pReceiver<'ch, M, TXN, RXN>)
where
    M: RawMutex,
{
    let sender = P2pSender {
        can: can.clone(),
        local,
    };
    let receiver = P2pReceiver {
        can,
        local,
        promiscuous: false,
        app: core::array::from_fn(|_| PeerReassembly::new()),
        control: core::array::from_fn(|_| PeerReassembly::new()),
        timesync: core::array::from_fn(|_| PeerReassembly::new()),
        arbitration: core::array::from_fn(|_| PeerReassembly::new()),
    };
    (sender, receiver)
}

/// Builds an [`RxFilter`] that accepts, at the MCP25625 hardware level, only
/// frames addressed to `local` or broadcast to every peer — across every
/// channel (public or private) at once, since the mask only inspects the
/// broadcast flag and `local`'s own bit position in the receiver field,
/// leaving the sender field, the control/timesync flags, and every other
/// peer's receiver bit as "don't care".
///
/// Without this, every frame on the bus — including ones addressed to other
/// peers — still reaches [`P2pReceiver::receive`], which discards them in
/// software only after each has already crossed SPI and raised an
/// interrupt; this instead lets the controller drop them before either
/// happens. Pass the result to [`crate::Mcp25625Config::with_filter`] before
/// [`crate::Mcp25625::init`].
///
/// This complements, rather than replaces, [`P2pReceiver::receive`]'s own
/// software check — that check alone is always correct by itself (see the
/// module docs), so leaving the device on [`crate::RxFilter::AcceptAll`]
/// instead of this is always an option, just a noisier one (every frame on
/// the bus reaches software, not just this peer's own).
pub fn hardware_filter(local: PeerAddr) -> RxFilter {
    let local_bit = 1u32 << local.as_u8();
    let mask_raw = BROADCAST_BIT | local_bit;
    let unicast_raw = local_bit; // broadcast=0, my receiver bit=1
    let broadcast_raw = BROADCAST_BIT; // broadcast=1, receiver field=0
    let wrap = |raw: u32| {
        Id::Extended(
            ExtendedId::new(raw).expect("mask/filter values fit within 29 bits by construction"),
        )
    };
    RxFilter::Masked {
        mask: wrap(mask_raw),
        ids: Vec::from_slice(&[wrap(unicast_raw), wrap(broadcast_raw)])
            .expect("2 filter values fit within the 6-slot capacity"),
    }
}

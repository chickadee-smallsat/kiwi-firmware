//! A peer membership, role-registration, and bus-access coordination
//! protocol layered on [`crate::p2p`] — "bus arbitration" in the sense of
//! deciding, at the application level, who's allowed to do what, not in the
//! CAN sense (CAN already arbitrates physical bus access bit-by-bit; this is
//! a separate, higher-level concern).
//!
//! One peer (the "arbiter") tracks a directory mapping each registered peer
//! address to a [`Role`] (an application-chosen 6-byte label, e.g. `b"IMU\0\0\0"`),
//! hands out addresses to peers that don't have one yet, answers directory
//! queries, and grants short-lived exclusive bus-access reservations
//! (Request-To-Send/Clear-To-Send) on request. Every other peer is a
//! follower that listens for the arbiter's presence, and — via [`bootstrap`]
//! — asks it for an address before doing anything else.
//!
//! # Reserved addresses
//!
//! [`UNASSIGNED_ADDR`] and [`ARBITER_ADDR`] are carved out of [`PeerAddr`]'s
//! normal range: ordinary, already-registered peers use [`ARBITER_ADDR`] as
//! the [`bootstrap`]-time destination when no arbiter has been heard from
//! yet (useful for a permanently pinned arbiter that always uses this
//! address as its own — see [`BusArbiter`]), while every not-yet-registered
//! device uses [`UNASSIGNED_ADDR`] as its own sender address. That means
//! [`ASSIGNABLE_PEERS`] (10, not [`MAX_PEERS`]'s full 12) addresses are ever
//! actually handed out to a registered role.
//!
//! Sharing one sender address among every unassigned device is only safe
//! because [`bootstrap`] requests are short, retried on timeout, and tagged
//! with a random `salt` the caller supplies: if two devices happen to
//! request at once, [`P2pReceiver`]'s single reassembly slot for
//! [`UNASSIGNED_ADDR`] can only successfully reassemble one of them at a
//! time (the second corrupts and drops, per [`crate::p2p`]'s usual
//! best-effort handling) — but the salt lets each device recognize whether
//! a given [`RegistrationOutcome`] response was actually meant for *it*
//! rather than a different, similarly unlucky device, and the request is
//! cheap enough to just retry after a short backoff. This is a deliberate
//! trade: a small chance of a slower bootstrap under contention, in
//! exchange for not needing every device to already have a unique address
//! before it can ask for one.
//!
//! # Wire channel
//!
//! Every message in this module travels on a private wire channel with no
//! [`crate::P2pChannel`] counterpart, the same way `crate::timesync` does —
//! see that module's and [`crate::p2p`]'s docs for why. Dispatching messages
//! on this channel (and `crate::timesync`'s) is handled by
//! [`crate::ProtocolRunner`], the sole consumer of a node's [`P2pReceiver`];
//! it can be configured for bus arbitration, time sync, both at once, or
//! neither.
//!
//! # Election
//!
//! If a peer hasn't heard a [`BusArbiter`] heartbeat in a while, it can
//! nominate itself by broadcasting a random priority; every peer that hears
//! any nomination remembers the highest `(priority, sender address)` pair
//! seen (address breaks ties, so exactly one nomination always wins, never a
//! draw). A peer that nominated itself and is still the best choice once its
//! own collection window closes becomes the arbiter and starts sending
//! heartbeats; see [`run_arbiter_supervisor`], which drives this whole cycle
//! as a standalone task.
//!
//! # Getting started
//!
//! 1. Build a temporary [`P2pSender`]/[`P2pReceiver`] pair with
//!    [`crate::new_p2p`] using [`UNASSIGNED_ADDR`] as the local address, and
//!    call [`bootstrap`] on it to get a real [`PeerAddr`] and confirm a
//!    [`Role`].
//! 2. Drop that pair and build the real one with [`crate::new_p2p`] using
//!    the address [`bootstrap`] returned — this is the pair the application
//!    uses from then on for [`crate::P2pChannel::App`]/[`crate::P2pChannel::Control`]
//!    traffic, and the one [`crate::ProtocolRunner`] takes ownership of.
//! 3. Spawn [`crate::ProtocolRunner::run`] (owns the receiver; forwards
//!    `App`/`Control` traffic through the channel [`crate::ProtocolRunner::new`]
//!    returns) configured with a [`SharedBusArbitrationPeer`] and
//!    [`SharedBusArbiterRole`], and [`run_arbiter_supervisor`] (independent
//!    task; elects and, if elected, heartbeats) against that same pair.
//! 4. Use [`BusArbitrationPeer::request_peer_list`]/[`BusArbitrationPeer::request_rts`]
//!    whenever the application wants the current directory or a bus-access
//!    reservation; poll [`BusArbitrationPeer::arbiter`]/[`BusArbitrationPeer::directory`]
//!    to read the results.

use core::cell::RefCell;

use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::{raw::RawMutex, Mutex};
use embedded_hal_async::delay::DelayNs;
use heapless::Vec;

use crate::{
    p2p::{
        Destination, P2pError, P2pReceiver, P2pSender, PeerAddr, WireChannel,
        MAX_ARBITRATION_MSG_LEN, MAX_PEERS,
    },
    timesync::Clock,
};

/// Well-known address every not-yet-registered device uses as its own
/// sender address — see the module docs on why sharing this is safe.
pub const UNASSIGNED_ADDR: PeerAddr = PeerAddr::Peer11;
/// Well-known address reserved for a permanently pinned arbiter (one
/// configured to always hold the role, using this as its own `local`
/// address, rather than winning an election). Also used as [`bootstrap`]'s
/// destination before any heartbeat has been heard. Not specially enforced
/// anywhere in this module beyond being excluded from [`ASSIGNABLE_PEERS`];
/// an elected (non-pinned) arbiter just uses its own ordinary address, and
/// followers learn it from its heartbeats instead.
pub const ARBITER_ADDR: PeerAddr = PeerAddr::Peer10;
/// Number of addresses this module ever actually hands out to a registered
/// role: [`MAX_PEERS`] minus the two reserved above.
pub const ASSIGNABLE_PEERS: usize = MAX_PEERS - 2;

/// An application-chosen label identifying a registered peer's purpose
/// (e.g. `b"IMU\0\0\0"`), fixed at 6 bytes so it always fits alongside a
/// salt and kind byte in a single CAN frame — see the module docs on why
/// that matters for [`bootstrap`] requests specifically.
pub type Role = [u8; 6];

const KIND_HEARTBEAT: u8 = 0;
const KIND_CANDIDACY: u8 = 1;
const KIND_ID_REQUEST: u8 = 2;
const KIND_ID_RESPONSE: u8 = 3;
const KIND_PEER_LIST_REQUEST: u8 = 4;
const KIND_PEER_LIST_RESPONSE: u8 = 5;
const KIND_RTS_REQUEST: u8 = 6;
const KIND_CTS_RESPONSE: u8 = 7;

/// The result of a registration request.
/// Registers `role` to the lowest free address, unless `role` is
/// already registered (→ [`RegistrationOutcome::RoleTaken`]) or every
/// address is taken (→ [`RegistrationOutcome::NoIdsAvailable`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum RegistrationOutcome {
    /// The request succeeded: `addr` is now registered to `role`.
    Granted {
        /// The newly assigned address.
        addr: PeerAddr,
        /// The role it was registered under (an echo of what was requested).
        role: Role,
    },
    /// `role` is already registered to some address.
    RoleTaken,
    /// Every one of the [`ASSIGNABLE_PEERS`] addresses is already registered.
    NoIdsAvailable,
}

/// The messages this protocol exchanges, all on the private wire channel
/// described in the module docs. Not `Copy`: [`Self::PeerListResponse`]
/// holds a `Vec`. `pub(crate)` (rather than fully private) only so
/// [`crate::ProtocolRunner`] can encode the reply [`BusArbiter::handle_sync`]
/// hands it, without needing to `.await` a send while still holding the
/// blocking lock around a [`BusArbiter`] — see that method's docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BusArbitrationMessage {
    /// Broadcast periodically by the current arbiter.
    Heartbeat,
    /// Broadcast by a peer nominating itself during an election, carrying a
    /// random tie-break value (see the module docs).
    Candidacy { priority: u32 },
    /// Sent to the arbiter (see [`bootstrap`]) requesting registration.
    IdRequest { salt: u16, role: Role },
    /// The arbiter's reply to an [`Self::IdRequest`].
    IdResponse {
        salt: u16,
        result: RegistrationOutcome,
    },
    /// Sent to the arbiter asking for the current directory.
    PeerListRequest { salt: u16 },
    /// The arbiter's reply to a [`Self::PeerListRequest`].
    PeerListResponse {
        salt: u16,
        entries: Vec<(PeerAddr, Role), ASSIGNABLE_PEERS>,
    },
    /// Sent to the arbiter requesting exclusive bus access for `duration`
    /// (in the same opaque units as this module's [`Clock`] values).
    RtsRequest { salt: u16, duration: u16 },
    /// The arbiter's reply to an [`Self::RtsRequest`].
    CtsResponse { salt: u16, granted: bool },
}

impl BusArbitrationMessage {
    pub(crate) fn encode(&self) -> ([u8; MAX_ARBITRATION_MSG_LEN], usize) {
        let mut buf = [0u8; MAX_ARBITRATION_MSG_LEN];
        let len = match self {
            Self::Heartbeat => {
                buf[0] = KIND_HEARTBEAT;
                1
            }
            Self::Candidacy { priority } => {
                buf[0] = KIND_CANDIDACY;
                buf[1..5].copy_from_slice(&priority.to_be_bytes());
                5
            }
            Self::IdRequest { salt, role } => {
                buf[0] = KIND_ID_REQUEST;
                buf[1..3].copy_from_slice(&salt.to_be_bytes());
                buf[3..9].copy_from_slice(role);
                9
            }
            Self::IdResponse { salt, result } => {
                buf[0] = KIND_ID_RESPONSE;
                buf[1..3].copy_from_slice(&salt.to_be_bytes());
                match result {
                    RegistrationOutcome::Granted { addr, role } => {
                        buf[3] = 0;
                        buf[4] = addr.as_u8();
                        buf[5..11].copy_from_slice(role);
                        11
                    }
                    RegistrationOutcome::RoleTaken => {
                        buf[3] = 1;
                        4
                    }
                    RegistrationOutcome::NoIdsAvailable => {
                        buf[3] = 2;
                        4
                    }
                }
            }
            Self::PeerListRequest { salt } => {
                buf[0] = KIND_PEER_LIST_REQUEST;
                buf[1..3].copy_from_slice(&salt.to_be_bytes());
                3
            }
            Self::PeerListResponse { salt, entries } => {
                buf[0] = KIND_PEER_LIST_RESPONSE;
                buf[1..3].copy_from_slice(&salt.to_be_bytes());
                buf[3] = entries.len() as u8;
                let mut offset = 4;
                for (addr, role) in entries {
                    buf[offset] = addr.as_u8();
                    buf[offset + 1..offset + 7].copy_from_slice(role);
                    offset += 7;
                }
                offset
            }
            Self::RtsRequest { salt, duration } => {
                buf[0] = KIND_RTS_REQUEST;
                buf[1..3].copy_from_slice(&salt.to_be_bytes());
                buf[3..5].copy_from_slice(&duration.to_be_bytes());
                5
            }
            Self::CtsResponse { salt, granted } => {
                buf[0] = KIND_CTS_RESPONSE;
                buf[1..3].copy_from_slice(&salt.to_be_bytes());
                buf[3] = *granted as u8;
                4
            }
        };
        (buf, len)
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let (&kind, rest) = bytes.split_first()?;
        match kind {
            KIND_HEARTBEAT if rest.is_empty() => Some(Self::Heartbeat),
            KIND_CANDIDACY if rest.len() == 4 => Some(Self::Candidacy {
                priority: u32::from_be_bytes(rest.try_into().unwrap()),
            }),
            KIND_ID_REQUEST if rest.len() == 8 => {
                let salt = u16::from_be_bytes([rest[0], rest[1]]);
                let mut role = [0u8; 6];
                role.copy_from_slice(&rest[2..8]);
                Some(Self::IdRequest { salt, role })
            }
            KIND_ID_RESPONSE if rest.len() >= 3 => {
                let salt = u16::from_be_bytes([rest[0], rest[1]]);
                let result = match (rest[2], rest.len()) {
                    (0, 10) => {
                        let addr = PeerAddr::new(rest[3])?;
                        let mut role = [0u8; 6];
                        role.copy_from_slice(&rest[4..10]);
                        RegistrationOutcome::Granted { addr, role }
                    }
                    (1, 3) => RegistrationOutcome::RoleTaken,
                    (2, 3) => RegistrationOutcome::NoIdsAvailable,
                    _ => return None,
                };
                Some(Self::IdResponse { salt, result })
            }
            KIND_PEER_LIST_REQUEST if rest.len() == 2 => Some(Self::PeerListRequest {
                salt: u16::from_be_bytes([rest[0], rest[1]]),
            }),
            KIND_PEER_LIST_RESPONSE if rest.len() >= 3 => {
                let salt = u16::from_be_bytes([rest[0], rest[1]]);
                let count = rest[2] as usize;
                if count > ASSIGNABLE_PEERS || rest.len() != 3 + count * 7 {
                    return None;
                }
                let mut entries = Vec::new();
                for i in 0..count {
                    let off = 3 + i * 7;
                    let addr = PeerAddr::new(rest[off])?;
                    let mut role = [0u8; 6];
                    role.copy_from_slice(&rest[off + 1..off + 7]);
                    entries.push((addr, role)).ok()?;
                }
                Some(Self::PeerListResponse { salt, entries })
            }
            KIND_RTS_REQUEST if rest.len() == 4 => {
                let salt = u16::from_be_bytes([rest[0], rest[1]]);
                let duration = u16::from_be_bytes([rest[2], rest[3]]);
                Some(Self::RtsRequest { salt, duration })
            }
            KIND_CTS_RESPONSE if rest.len() == 3 => Some(Self::CtsResponse {
                salt: u16::from_be_bytes([rest[0], rest[1]]),
                granted: rest[2] != 0,
            }),
            _ => None,
        }
    }

    /// The destination a reply to a message from `src` should use: a
    /// still-unassigned requester shares [`UNASSIGNED_ADDR`], so a reply can
    /// only ever reach it as a broadcast (see the module docs); an already
    /// registered requester gets a plain unicast reply.
    fn reply_dest(src: PeerAddr) -> Destination {
        if src == UNASSIGNED_ADDR {
            Destination::Broadcast
        } else {
            Destination::Peer(src)
        }
    }
}

/// The arbiter role: tracks the peer directory and the current RTS/CTS
/// grant, and answers requests. Only one peer on the bus should hold this
/// at a time — see [`run_arbiter_supervisor`] for how a peer decides
/// whether that's itself.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct BusArbiter {
    directory: [Option<Role>; ASSIGNABLE_PEERS],
    /// The current exclusive-access holder and when its grant expires (in
    /// [`Clock`] units), if any.
    rts: Option<(PeerAddr, u64)>,
}

impl BusArbiter {
    /// Creates an arbiter with an empty directory and no active RTS grant.
    pub const fn new() -> Self {
        Self {
            directory: [None; ASSIGNABLE_PEERS],
            rts: None,
        }
    }

    /// The current directory: `directory()[i]` is the role registered to
    /// address `i` (see [`PeerAddr::as_u8`]), or `None` if that address is
    /// still free.
    pub const fn directory(&self) -> &[Option<Role>; ASSIGNABLE_PEERS] {
        &self.directory
    }

    /// Registers `role` to the lowest free address, unless `role` is
    /// already registered (→ [`RegistrationOutcome::RoleTaken`]) or every
    /// address is taken (→ [`RegistrationOutcome::NoIdsAvailable`]).
    fn register(&mut self, role: Role) -> RegistrationOutcome {
        if self.directory.contains(&Some(role)) {
            return RegistrationOutcome::RoleTaken;
        }
        match self.directory.iter().position(Option::is_none) {
            Some(idx) => {
                self.directory[idx] = Some(role);
                RegistrationOutcome::Granted {
                    addr: PeerAddr::new(idx as u8).expect("idx < ASSIGNABLE_PEERS <= MAX_PEERS"),
                    role,
                }
            }
            None => RegistrationOutcome::NoIdsAvailable,
        }
    }

    /// Synchronous half of [`Self::handle_message`]: decodes and acts on
    /// `payload` if it's a request this arbiter answers (an
    /// [`BusArbitrationMessage::IdRequest`]/`PeerListRequest`/`RtsRequest`),
    /// returning the reply to send (and to whom) — or `None` if `payload`
    /// wasn't one of those (including simply failing to decode). Split out
    /// so a caller sharing this behind a blocking lock (see
    /// [`SharedBusArbiterRole`]) — [`crate::ProtocolRunner`] — only needs to
    /// hold it for this instant state update, `.await`ing the actual send
    /// afterwards without holding it. `pub(crate)` rather than fully private
    /// for exactly that caller, which lives in a different module.
    pub(crate) fn handle_sync(
        &mut self,
        src: PeerAddr,
        payload: &[u8],
        now: u64,
    ) -> Option<(Destination, BusArbitrationMessage)> {
        let dest = BusArbitrationMessage::reply_dest(src);
        match BusArbitrationMessage::decode(payload)? {
            BusArbitrationMessage::IdRequest { salt, role } => {
                let result = self.register(role);
                Some((dest, BusArbitrationMessage::IdResponse { salt, result }))
            }
            BusArbitrationMessage::PeerListRequest { salt } => {
                let mut entries = Vec::new();
                for (i, slot) in self.directory.iter().enumerate() {
                    if let Some(role) = slot {
                        let addr =
                            PeerAddr::new(i as u8).expect("i < ASSIGNABLE_PEERS <= MAX_PEERS");
                        // Capacity is exactly ASSIGNABLE_PEERS, the same as
                        // `directory`, so this never overflows.
                        let _ = entries.push((addr, *role));
                    }
                }
                Some((
                    dest,
                    BusArbitrationMessage::PeerListResponse { salt, entries },
                ))
            }
            BusArbitrationMessage::RtsRequest { salt, duration } => {
                let granted = match self.rts {
                    Some((_, expiry)) if now < expiry => false,
                    _ => {
                        self.rts = Some((src, now + duration as u64));
                        true
                    }
                };
                Some((dest, BusArbitrationMessage::CtsResponse { salt, granted }))
            }
            BusArbitrationMessage::Heartbeat
            | BusArbitrationMessage::Candidacy { .. }
            | BusArbitrationMessage::IdResponse { .. }
            | BusArbitrationMessage::PeerListResponse { .. }
            | BusArbitrationMessage::CtsResponse { .. } => None,
        }
    }

    /// Processes one message just received from `src`, replying if it was a
    /// request this arbiter answers. Returns `true` if it was (whether or
    /// not the reply send itself succeeded), `false` otherwise.
    ///
    /// `now` must be this node's own clock reading, in the same unit and
    /// epoch used for RTS grant durations.
    pub async fn handle_message<'ch, M, const TXN: usize, const RXN: usize>(
        &mut self,
        sender: &P2pSender<'ch, M, TXN, RXN>,
        src: PeerAddr,
        payload: &[u8],
        now: u64,
    ) -> Result<bool, P2pError>
    where
        M: RawMutex,
    {
        match self.handle_sync(src, payload, now) {
            Some((dest, resp)) => {
                let (buf, len) = resp.encode();
                sender.send_arbitration(dest, &buf[..len]).await?;
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

impl Default for BusArbiter {
    fn default() -> Self {
        Self::new()
    }
}

/// A [`BusArbiter`] behind a synchronous lock, so [`crate::ProtocolRunner`]
/// (processing incoming requests) and [`run_arbiter_supervisor`] (deciding
/// whether this node holds the role at all) can share it. `None` means this
/// node is not currently the arbiter.
pub type SharedBusArbiterRole<M> = Mutex<M, RefCell<Option<BusArbiter>>>;

/// An event [`BusArbitrationPeer::handle_message`] reports back after
/// successfully processing a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum BusArbitrationEvent {
    /// A heartbeat revealed a new (or first) known arbiter address.
    ArbiterChanged(PeerAddr),
    /// [`BusArbitrationPeer::directory`] was refreshed from a
    /// [`BusArbitrationPeer::request_peer_list`] response.
    PeerListUpdated,
    /// An outstanding [`BusArbitrationPeer::request_rts`] was granted.
    RtsGranted,
    /// An outstanding [`BusArbitrationPeer::request_rts`] was denied.
    RtsDenied,
}

/// Tracks a peer's view of bus arbitration: the last known arbiter, a
/// cached copy of the directory, in-flight election bookkeeping, and
/// outstanding request salts. Held by every peer, including whichever one
/// currently also holds a [`BusArbiter`] (the two are independent — nothing
/// stops the arbiter from also tracking its own view this way, though it
/// has no real need to query itself).
///
/// Registration itself (getting a first address at all) isn't tracked
/// here — see [`bootstrap`], a one-shot operation performed before this
/// struct or [`crate::ProtocolRunner`] are even in the picture.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct BusArbitrationPeer {
    arbiter: Option<PeerAddr>,
    last_heartbeat: Option<u64>,
    directory_cache: [Option<Role>; ASSIGNABLE_PEERS],
    pending_peer_list: Option<u16>,
    pending_rts: Option<u16>,
    /// Best `(priority, address)` nomination seen so far this election
    /// round, if one is in progress — see [`run_arbiter_supervisor`].
    election: Option<(u32, PeerAddr)>,
}

impl BusArbitrationPeer {
    /// Creates a peer with no arbiter known yet and an empty directory cache.
    pub const fn new() -> Self {
        Self {
            arbiter: None,
            last_heartbeat: None,
            directory_cache: [None; ASSIGNABLE_PEERS],
            pending_peer_list: None,
            pending_rts: None,
            election: None,
        }
    }

    /// The last address a heartbeat was heard from, or `None` if none has
    /// been heard yet.
    pub const fn arbiter(&self) -> Option<PeerAddr> {
        self.arbiter
    }

    /// The most recently cached directory, from the last completed
    /// [`Self::request_peer_list`] — not automatically kept up to date
    /// otherwise.
    pub const fn directory(&self) -> &[Option<Role>; ASSIGNABLE_PEERS] {
        &self.directory_cache
    }

    fn last_heartbeat(&self) -> Option<u64> {
        self.last_heartbeat
    }

    fn note_candidacy(&mut self, src: PeerAddr, priority: u32) {
        let candidate = (priority, src.as_u8());
        let better = match self.election {
            Some((best_priority, best_addr)) => candidate > (best_priority, best_addr.as_u8()),
            None => true,
        };
        if better {
            self.election = Some((priority, src));
        }
    }

    fn is_election_leader(&self, priority: u32, local: PeerAddr) -> bool {
        self.election == Some((priority, local))
    }

    fn clear_election(&mut self) {
        self.election = None;
    }

    /// Feeds a message just received over the private arbitration channel
    /// (from `src`) into this peer's bookkeeping.
    ///
    /// `now` must be this node's own clock reading, taken as close as
    /// possible to when `payload` was received.
    pub fn handle_message(
        &mut self,
        src: PeerAddr,
        payload: &[u8],
        now: u64,
    ) -> Option<BusArbitrationEvent> {
        match BusArbitrationMessage::decode(payload)? {
            BusArbitrationMessage::Heartbeat => {
                self.last_heartbeat = Some(now);
                if self.arbiter == Some(src) {
                    None
                } else {
                    self.arbiter = Some(src);
                    Some(BusArbitrationEvent::ArbiterChanged(src))
                }
            }
            BusArbitrationMessage::Candidacy { priority } => {
                self.note_candidacy(src, priority);
                None
            }
            BusArbitrationMessage::PeerListResponse { salt, entries } => {
                if self.pending_peer_list != Some(salt) {
                    return None;
                }
                self.pending_peer_list = None;
                self.directory_cache = [None; ASSIGNABLE_PEERS];
                for (addr, role) in entries {
                    self.directory_cache[addr.as_u8() as usize] = Some(role);
                }
                Some(BusArbitrationEvent::PeerListUpdated)
            }
            BusArbitrationMessage::CtsResponse { salt, granted } => {
                if self.pending_rts != Some(salt) {
                    return None;
                }
                self.pending_rts = None;
                Some(if granted {
                    BusArbitrationEvent::RtsGranted
                } else {
                    BusArbitrationEvent::RtsDenied
                })
            }
            // Arbiter-bound requests, and IdResponse (handled directly by
            // `bootstrap` instead, before this struct is even in use).
            BusArbitrationMessage::IdRequest { .. }
            | BusArbitrationMessage::IdResponse { .. }
            | BusArbitrationMessage::PeerListRequest { .. }
            | BusArbitrationMessage::RtsRequest { .. } => None,
        }
    }

    /// Sends a directory query to the last known arbiter (or
    /// [`ARBITER_ADDR`] if none has been heard from yet), remembering `salt`
    /// so the eventual response updates [`Self::directory`] — see
    /// [`Self::handle_message`].
    pub async fn request_peer_list<'ch, M, const TXN: usize, const RXN: usize>(
        &mut self,
        sender: &P2pSender<'ch, M, TXN, RXN>,
        salt: u16,
    ) -> Result<(), P2pError>
    where
        M: RawMutex,
    {
        self.pending_peer_list = Some(salt);
        let (buf, len) = BusArbitrationMessage::PeerListRequest { salt }.encode();
        sender
            .send_arbitration(self.arbiter.unwrap_or(ARBITER_ADDR), &buf[..len])
            .await
    }

    /// Requests exclusive bus access for `duration` (in the same opaque
    /// units as this module's [`Clock`] values) from the last known arbiter
    /// (or [`ARBITER_ADDR`] if none has been heard from yet), remembering
    /// `salt` so the eventual response is reported via
    /// [`Self::handle_message`].
    pub async fn request_rts<'ch, M, const TXN: usize, const RXN: usize>(
        &mut self,
        sender: &P2pSender<'ch, M, TXN, RXN>,
        salt: u16,
        duration: u16,
    ) -> Result<(), P2pError>
    where
        M: RawMutex,
    {
        self.pending_rts = Some(salt);
        let (buf, len) = BusArbitrationMessage::RtsRequest { salt, duration }.encode();
        sender
            .send_arbitration(self.arbiter.unwrap_or(ARBITER_ADDR), &buf[..len])
            .await
    }
}

/// A [`BusArbitrationPeer`] behind a synchronous lock, so
/// [`crate::ProtocolRunner`] (updating it) and [`run_arbiter_supervisor`]
/// (reading heartbeat staleness, running elections) — and any other task
/// that wants to call [`BusArbitrationPeer::request_peer_list`]/
/// [`BusArbitrationPeer::request_rts`] or read its accessors — can all share
/// it. The blocking (not async) mutex is deliberate, the same way it is for
/// [`crate::SharedTimeSyncFollower`]: every operation here is instant local
/// state manipulation, never I/O.
pub type SharedBusArbitrationPeer<M> = Mutex<M, RefCell<BusArbitrationPeer>>;

/// Performs one-time registration for a not-yet-assigned device: sends a
/// request for `role` and waits for a matching
/// response, retrying with a fresh salt (from `next_salt`) up to
/// `max_attempts` times if none arrives within `retry_timeout` of sending.
///
/// `receiver`/`sender` must be a [`P2pSender`]/[`P2pReceiver`] pair built
/// with [`UNASSIGNED_ADDR`] as the local address (see the module docs) —
/// this function is meant to run *before* the application builds its real,
/// permanently-addressed pair, and is the sole consumer of `receiver` for
/// its own duration (like any other [`P2pReceiver`] user).
///
/// Returns `None` if every attempt timed out, or if the arbiter rejected the
/// request (`role` already taken, or every address already handed out) —
/// these aren't distinguished, since retrying with the same `role` would
/// fail identically either way; a caller that cares can decode the reason
/// itself by not using this helper.
pub async fn bootstrap<'ch, M, const TXN: usize, const RXN: usize, D>(
    receiver: &mut P2pReceiver<'ch, M, TXN, RXN>,
    sender: &P2pSender<'ch, M, TXN, RXN>,
    role: Role,
    mut next_salt: impl FnMut() -> u16,
    mut delay: D,
    retry_timeout_ms: u32,
    max_attempts: u32,
) -> Option<(PeerAddr, Role)>
where
    M: RawMutex,
    D: DelayNs,
{
    let mut known_arbiter: Option<PeerAddr> = None;
    for _ in 0..max_attempts {
        let salt = next_salt();
        let dest = known_arbiter.unwrap_or(ARBITER_ADDR);
        let (buf, len) = BusArbitrationMessage::IdRequest { salt, role }.encode();
        if sender.send_arbitration(dest, &buf[..len]).await.is_err() {
            continue;
        }
        match select(
            wait_for_id_response(receiver, salt, &mut known_arbiter),
            delay.delay_ms(retry_timeout_ms),
        )
        .await
        {
            Either::First(RegistrationOutcome::Granted { addr, role }) => {
                return Some((addr, role));
            }
            Either::First(RegistrationOutcome::RoleTaken | RegistrationOutcome::NoIdsAvailable) => {
                return None;
            }
            Either::Second(()) => {} // timed out; retry
        }
    }
    None
}

/// Waits for either a matching [`BusArbitrationMessage::IdResponse`] (by
/// `salt`) or a heartbeat (recorded into `known_arbiter` so later retries
/// can address it directly instead of [`ARBITER_ADDR`]) — see [`bootstrap`].
async fn wait_for_id_response<'ch, M, const TXN: usize, const RXN: usize>(
    receiver: &mut P2pReceiver<'ch, M, TXN, RXN>,
    salt: u16,
    known_arbiter: &mut Option<PeerAddr>,
) -> RegistrationOutcome
where
    M: RawMutex,
{
    loop {
        let (src, channel, payload) = receiver.receive_raw().await;
        if channel != WireChannel::Arbitration {
            continue;
        }
        match BusArbitrationMessage::decode(&payload) {
            Some(BusArbitrationMessage::Heartbeat) => *known_arbiter = Some(src),
            Some(BusArbitrationMessage::IdResponse {
                salt: resp_salt,
                result,
            }) if resp_salt == salt => return result,
            _ => {}
        }
    }
}

/// Runs forever as the independent task that decides whether this node is
/// the arbiter: if no heartbeat has been seen for `heartbeat_timeout_ms`
/// (checked via `peer`, kept up to date by [`crate::ProtocolRunner`]) and
/// this node isn't already the arbiter, it broadcasts a candidacy with a
/// fresh priority from `priority_source`, waits `election_window_ms` for
/// competing candidacies (also tracked via `peer`), and becomes the arbiter
/// — installing a fresh [`BusArbiter`] into `arbiter_role` and looping to
/// broadcast heartbeats every `heartbeat_period_ms` — if its own priority
/// (tie-broken by address) was still the best once the window closes.
///
/// Meant to be spawned as its own task, independent of whatever task is
/// running [`crate::ProtocolRunner`] — see the module docs on why sending
/// needs no coordination with whatever's consuming this bus's incoming p2p
/// traffic.
#[allow(clippy::too_many_arguments)]
pub async fn run_arbiter_supervisor<'ch, 'p, 'ar, M, const TXN: usize, const RXN: usize, D, C>(
    peer: &'p SharedBusArbitrationPeer<M>,
    arbiter_role: &'ar SharedBusArbiterRole<M>,
    sender: P2pSender<'ch, M, TXN, RXN>,
    local: PeerAddr,
    mut delay: D,
    heartbeat_timeout_ms: u32,
    election_window_ms: u32,
    heartbeat_period_ms: u32,
    mut clock: C,
    mut priority_source: impl FnMut() -> u32,
) -> !
where
    M: RawMutex,
    D: DelayNs,
    C: Clock,
{
    loop {
        delay.delay_ms(heartbeat_timeout_ms).await;

        let already_arbiter = arbiter_role.lock(|cell| cell.borrow().is_some());
        if already_arbiter {
            continue;
        }
        let now = clock.now();
        let stale = peer.lock(|cell| match cell.borrow().last_heartbeat() {
            Some(t) => now.saturating_sub(t) >= heartbeat_timeout_ms as u64,
            None => true,
        });
        if !stale {
            continue;
        }

        let priority = priority_source();
        peer.lock(|cell| cell.borrow_mut().note_candidacy(local, priority));
        let (buf, len) = BusArbitrationMessage::Candidacy { priority }.encode();
        let _ = sender.broadcast_arbitration(&buf[..len]).await;
        delay.delay_ms(election_window_ms).await;

        let won = peer.lock(|cell| {
            let mut p = cell.borrow_mut();
            let won = p.is_election_leader(priority, local);
            p.clear_election();
            won
        });
        if !won {
            continue;
        }

        arbiter_role.lock(|cell| *cell.borrow_mut() = Some(BusArbiter::new()));
        loop {
            let (buf, len) = BusArbitrationMessage::Heartbeat.encode();
            let _ = sender.broadcast_arbitration(&buf[..len]).await;
            delay.delay_ms(heartbeat_period_ms).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_encode_decode_roundtrip() {
        let messages = [
            BusArbitrationMessage::Heartbeat,
            BusArbitrationMessage::Candidacy {
                priority: 0xDEAD_BEEF,
            },
            BusArbitrationMessage::IdRequest {
                salt: 0x1234,
                role: *b"IMU\0\0\0",
            },
            BusArbitrationMessage::IdResponse {
                salt: 0x1234,
                result: RegistrationOutcome::Granted {
                    addr: PeerAddr::new(3).unwrap(),
                    role: *b"IMU\0\0\0",
                },
            },
            BusArbitrationMessage::IdResponse {
                salt: 0x1234,
                result: RegistrationOutcome::RoleTaken,
            },
            BusArbitrationMessage::PeerListRequest { salt: 7 },
            BusArbitrationMessage::RtsRequest {
                salt: 9,
                duration: 500,
            },
            BusArbitrationMessage::CtsResponse {
                salt: 9,
                granted: true,
            },
        ];
        for msg in messages {
            let (buf, len) = msg.encode();
            assert_eq!(BusArbitrationMessage::decode(&buf[..len]), Some(msg));
        }

        let mut entries: Vec<(PeerAddr, Role), ASSIGNABLE_PEERS> = Vec::new();
        entries
            .push((PeerAddr::new(0).unwrap(), *b"IMU\0\0\0"))
            .unwrap();
        entries
            .push((PeerAddr::new(2).unwrap(), *b"GPS\0\0\0"))
            .unwrap();
        let msg = BusArbitrationMessage::PeerListResponse { salt: 42, entries };
        let (buf, len) = msg.encode();
        assert_eq!(BusArbitrationMessage::decode(&buf[..len]), Some(msg));
    }

    #[test]
    fn registration_assigns_lowest_free_address_and_rejects_duplicates() {
        let mut arbiter = BusArbiter::new();
        let a = arbiter.register(*b"IMU\0\0\0");
        assert_eq!(
            a,
            RegistrationOutcome::Granted {
                addr: PeerAddr::new(0).unwrap(),
                role: *b"IMU\0\0\0"
            }
        );
        let b = arbiter.register(*b"GPS\0\0\0");
        assert_eq!(
            b,
            RegistrationOutcome::Granted {
                addr: PeerAddr::new(1).unwrap(),
                role: *b"GPS\0\0\0"
            }
        );
        // Same role again, even though a different address would otherwise
        // be free, is rejected.
        assert_eq!(
            arbiter.register(*b"IMU\0\0\0"),
            RegistrationOutcome::RoleTaken
        );
    }

    #[test]
    fn registration_exhausts_all_assignable_addresses() {
        let mut arbiter = BusArbiter::new();
        for i in 0..ASSIGNABLE_PEERS {
            let role = [b'A', b'0' + (i / 10) as u8, b'0' + (i % 10) as u8, 0, 0, 0];
            assert!(matches!(
                arbiter.register(role),
                RegistrationOutcome::Granted { .. }
            ));
        }
        assert_eq!(
            arbiter.register(*b"NOMORE"),
            RegistrationOutcome::NoIdsAvailable
        );
    }

    #[test]
    fn election_tie_break_is_deterministic_and_has_no_draws() {
        let mut peer = BusArbitrationPeer::new();
        let a = PeerAddr::new(2).unwrap();
        let b = PeerAddr::new(5).unwrap();

        // Equal priority: higher address wins, regardless of arrival order.
        peer.note_candidacy(a, 100);
        peer.note_candidacy(b, 100);
        assert!(peer.is_election_leader(100, b));
        assert!(!peer.is_election_leader(100, a));

        // Higher priority always wins even from a lower address.
        peer.clear_election();
        peer.note_candidacy(b, 100);
        peer.note_candidacy(a, 200);
        assert!(peer.is_election_leader(200, a));
    }

    #[test]
    fn rts_grant_blocks_until_expiry() {
        let mut arbiter = BusArbiter::new();
        let holder = PeerAddr::new(1).unwrap();
        let other = PeerAddr::new(2).unwrap();

        let (buf, len) = BusArbitrationMessage::RtsRequest {
            salt: 1,
            duration: 100,
        }
        .encode();
        let (_, first) = arbiter.handle_sync(holder, &buf[..len], 1000).unwrap();
        assert_eq!(
            first,
            BusArbitrationMessage::CtsResponse {
                salt: 1,
                granted: true
            }
        );

        // Still within the grant: a different peer is denied.
        let (buf, len) = BusArbitrationMessage::RtsRequest {
            salt: 2,
            duration: 50,
        }
        .encode();
        let (_, second) = arbiter.handle_sync(other, &buf[..len], 1050).unwrap();
        assert_eq!(
            second,
            BusArbitrationMessage::CtsResponse {
                salt: 2,
                granted: false
            }
        );

        // Past the expiry: available again.
        let (buf, len) = BusArbitrationMessage::RtsRequest {
            salt: 3,
            duration: 50,
        }
        .encode();
        let (_, third) = arbiter.handle_sync(other, &buf[..len], 1100).unwrap();
        assert_eq!(
            third,
            BusArbitrationMessage::CtsResponse {
                salt: 3,
                granted: true
            }
        );
    }
}

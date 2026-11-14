//! A PTP-like clock synchronization protocol layered on [`crate::p2p`]: one
//! peer (the "master") periodically broadcasts its own clock reading, and
//! every other peer (a "follower") passively listens and tracks its offset
//! from it — optionally refined by a Delay Request/Response exchange that
//! measures and cancels out one-way bus transit time, the same way IEEE 1588
//! PTP does.
//!
//! # Message flow
//!
//! ```text
//! master                                   follower
//!   |-- Sync{seq, t1} --------(broadcast)-->| records t2 = local receive time
//!   |                                       | offset := t1 - t2  (immediately usable)
//!   |                                       |
//!   |<-- DelayReq{seq} ---------(unicast)---| records t3 = local send time
//!   |-- DelayResp{seq, t4} ----(unicast)--->| offset, mean_path_delay refined
//! ```
//!
//! `t1` (master's Sync send time) and `t4` (master's Delay Request receive
//! time) are both master-clock readings; `t2` (follower's Sync receive
//! time) and `t3` (follower's Delay Request send time) are both
//! follower-clock readings. From all four:
//!
//! ```text
//! forward  = t2 - t1   (Sync transit time, as measured against both clocks' skew)
//! backward = t4 - t3   (Delay Request transit time, likewise)
//! offset          = (backward - forward) / 2   (add to a local reading to get master time)
//! mean_path_delay = (forward + backward) / 2   (one-way bus transit time, assumed symmetric)
//! ```
//!
//! This still isn't full IEEE 1588:
//!
//! - **Symmetric path assumed.** Like standard PTP, the two formulas above
//!   assume the Sync and Delay Request each took the same time to cross the
//!   bus. On a shared CAN bus that's a reasonable approximation (frame
//!   transmission time is fixed by bit rate and payload size in both
//!   directions) but bus contention could make one direction wait longer
//!   for arbitration than the other at any given moment.
//! - **No Best Master Clock Algorithm.** A follower trusts whichever peer it
//!   most recently heard a Sync from — there's no negotiation over which
//!   master is authoritative if more than one is broadcasting.
//! - **Software timestamps only.** `t1`..`t4` are whatever the application
//!   passes in as its own clock reading at the moment of each call — there
//!   is no hardware TX/RX timestamping, so each also carries whatever
//!   software scheduling jitter exists between "the frame actually went on
//!   the bus" and "the application read its clock".
//! - **Timestamps are opaque `u64`s.** This module doesn't know or assume a
//!   unit or epoch — it transports whatever the application's own clock
//!   produces (e.g. `embassy_time::Instant::as_ticks()`), as long as every
//!   peer uses the same unit and a compatible epoch.
//!
//! This protocol sends every message of its own on a dedicated, entirely
//! private wire channel — distinguished from ordinary p2p traffic (and from
//! the application's own [`crate::P2pChannel::App`]/[`crate::P2pChannel::Control`]
//! messages) by its own bit in the CAN ID, with no [`crate::P2pChannel`]
//! value naming it and no way to observe or produce it except through this
//! module — so [`crate::ProtocolRunner`] can tell time-sync messages apart
//! from application messages by which kind of CAN ID they arrived on, before
//! looking at their contents at all, rather than by trying to decode a
//! message and seeing whether that happens to succeed. That earlier,
//! payload-based approach had a real gap: an application message that
//! happened to be the same length as a `Sync`/`DelayResp` (and started with
//! a matching kind byte) would have been silently swallowed as a bogus
//! time-sync message — corrupting a follower's offset and never reaching
//! the application. Using a dedicated CAN ID bit closes that off entirely,
//! since it doesn't depend on what either side's payload bytes happen to
//! contain; it also gives time sync its own reassembly state per source
//! peer, so an interleaved message on this channel can no longer corrupt an
//! application message (or an application's own [`crate::P2pChannel::Control`]
//! protocol) — or vice versa — either. Making the channel unreachable
//! through [`crate::P2pChannel`] at all (rather than reusing `Control`, as
//! an earlier version of this module did) means an application's own
//! control-style protocol can use `Control` freely without any risk of
//! colliding with this one. [`TimeSyncFollower::handle_message`] and
//! [`TimeSyncMaster::handle_message`] still try to decode their `payload`
//! argument and return `None`/`Ok(None)` on failure, but only ever see
//! payloads [`crate::ProtocolRunner`] has already confirmed arrived on that
//! private channel — see [`crate::p2p`]'s module docs for the full
//! mechanism.
//!
//! # Running alongside other traffic
//!
//! [`P2pReceiver::receive`] must be the sole consumer of its handle's
//! incoming queue (see the `p2p` module docs), which shapes how time sync
//! can run alongside ordinary p2p traffic (and bus arbitration — see
//! `crate::busarb`): only one thing may ever call it. [`crate::ProtocolRunner`]
//! is that one thing — it owns the [`P2pReceiver`], and for every message it
//! receives, gives a configured follower and/or master first look (updating
//! synchronization state, and replying to Delay Requests) before forwarding
//! anything neither consumed on to the application via an ordinary queue,
//! unaffected. Its `run()` method is meant to be handed to a
//! `#[embassy_executor::task]` function defined at the call site, the same
//! way [`crate::Mcp25625Runner::run`] is spawned elsewhere in this workspace.
//!
//! Sending, in both directions, has no such restriction and so needs no
//! dedicated owner — [`TimeSyncMaster::run_broadcast`] and
//! [`run_periodic_delay_requests`] are each a self-contained loop suitable
//! for spawning as their own independent tasks too, using nothing but a
//! cloned [`crate::P2pSender`].
//!
//! A [`TimeSyncFollower`] driven this way needs to be reachable from
//! multiple tasks at once (the runner updates it; other tasks read
//! `offset()`/`to_master_time()`; a periodic task may call
//! [`request_delay_shared`]) — see [`SharedTimeSyncFollower`].

use core::cell::RefCell;

use embassy_sync::blocking_mutex::{raw::RawMutex, Mutex};
use embedded_hal_async::delay::DelayNs;
use heapless::Vec;

use crate::p2p::{P2pChannel, P2pError, P2pSender, PeerAddr, MAX_P2P_MSG_LEN};

/// A source of this node's own clock reading, in whatever unit and epoch
/// the application's time-sync deployment has agreed on (see the module
/// docs). Implemented for any `FnMut() -> u64`, e.g. a closure wrapping
/// `embassy_time::Instant::now().as_ticks()`.
pub trait Clock {
    /// Returns the current clock reading.
    fn now(&mut self) -> u64;
}

impl<F: FnMut() -> u64> Clock for F {
    fn now(&mut self) -> u64 {
        self()
    }
}

/// A [`TimeSyncFollower`] behind a synchronous lock, so it can be updated by
/// a [`crate::ProtocolRunner`] task and read (or have a delay request
/// initiated, via [`request_delay_shared`]) from any other task. Pick `M`
/// the same way you would for any other shared state in this workspace — a
/// `CriticalSectionRawMutex` if tasks might run on different cores, a
/// `NoopRawMutex` if they're all on the same executor.
///
/// The blocking (not async) mutex is deliberate: every operation on a
/// [`TimeSyncFollower`] is instant local state manipulation, never I/O, so
/// there's nothing to `.await` while holding the lock.
pub type SharedTimeSyncFollower<M> = Mutex<M, RefCell<TimeSyncFollower>>;

/// A p2p message [`crate::ProtocolRunner`] forwarded because it wasn't
/// consumed by time synchronization or bus arbitration — always
/// [`P2pChannel::App`] or [`P2pChannel::Control`], tagged accordingly, since
/// the private timesync/arbitration channels are always consumed and never
/// forwarded.
pub type ForwardedMessage = (PeerAddr, P2pChannel, Vec<u8, MAX_P2P_MSG_LEN>);

const KIND_SYNC: u8 = 0;
const KIND_DELAY_REQ: u8 = 1;
const KIND_DELAY_RESP: u8 = 2;
/// Largest wire encoding of any [`TimeSyncMessage`] (a `Sync` or `DelayResp`:
/// 1 kind byte + 1 sequence byte + an 8-byte big-endian timestamp).
const MAX_MSG_LEN: usize = 10;

/// This protocol's messages must fit in the private timesync channel's
/// reassembly buffer — enforced here so a future change to either constant
/// fails to compile instead of silently dropping every message this module
/// sends.
const _: () = assert!(MAX_MSG_LEN <= crate::p2p::MAX_TIMESYNC_MSG_LEN);

/// The three wire messages this protocol exchanges. Not exposed directly —
/// see [`TimeSync`] and [`DelayMeasurement`] for the data applications
/// actually consume, via [`TimeSyncMaster`] and [`TimeSyncFollower`].
enum TimeSyncMessage {
    /// Broadcast by the master: `timestamp` is its own clock reading at the
    /// moment of broadcast (`t1`).
    Sync { seq: u8, timestamp: u64 },
    /// Unicast by a follower to the master, to measure path delay.
    DelayReq { seq: u8 },
    /// Unicast by the master back to a follower: `t4` is the master's own
    /// clock reading at the moment it received the matching `DelayReq`.
    DelayResp { seq: u8, t4: u64 },
}

impl TimeSyncMessage {
    /// Encodes this message, returning the backing buffer and the number of
    /// leading bytes that are actually part of the encoding.
    fn encode(&self) -> ([u8; MAX_MSG_LEN], usize) {
        let mut buf = [0u8; MAX_MSG_LEN];
        let len = match *self {
            Self::Sync { seq, timestamp } => {
                buf[0] = KIND_SYNC;
                buf[1] = seq;
                buf[2..10].copy_from_slice(&timestamp.to_be_bytes());
                10
            }
            Self::DelayReq { seq } => {
                buf[0] = KIND_DELAY_REQ;
                buf[1] = seq;
                2
            }
            Self::DelayResp { seq, t4 } => {
                buf[0] = KIND_DELAY_RESP;
                buf[1] = seq;
                buf[2..10].copy_from_slice(&t4.to_be_bytes());
                10
            }
        };
        (buf, len)
    }

    /// Decodes a message from a p2p payload, or `None` if `bytes` doesn't
    /// match any known kind/length combination (and so clearly isn't one of
    /// these messages).
    fn decode(bytes: &[u8]) -> Option<Self> {
        let (&kind, rest) = bytes.split_first()?;
        let (&seq, rest) = rest.split_first()?;
        match (kind, rest.len()) {
            (KIND_SYNC, 8) => Some(Self::Sync {
                seq,
                timestamp: u64::from_be_bytes(rest.try_into().unwrap()),
            }),
            (KIND_DELAY_REQ, 0) => Some(Self::DelayReq { seq }),
            (KIND_DELAY_RESP, 8) => Some(Self::DelayResp {
                seq,
                t4: u64::from_be_bytes(rest.try_into().unwrap()),
            }),
            _ => None,
        }
    }
}

/// A Sync broadcast a [`TimeSyncFollower`] just processed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct TimeSync {
    /// The master's wrapping Sync sequence number.
    pub seq: u8,
    /// The master's own clock reading at the moment of broadcast (`t1`).
    pub timestamp: u64,
}

/// The result of a completed Delay Request/Response exchange, as computed
/// by [`TimeSyncFollower::handle_message`]. See the module docs for the
/// formulas and their symmetric-path assumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct DelayMeasurement {
    /// Refined offset: add to a local clock reading to get the master's
    /// equivalent clock reading.
    pub offset: i64,
    /// Estimated one-way bus transit time, in the same units as the
    /// timestamps exchanged. Can come out slightly negative in practice
    /// (clock drift within the round trip, or an asymmetric path); treat a
    /// wildly negative or implausibly large value as a sign the
    /// symmetric-path assumption didn't hold for that measurement.
    pub mean_path_delay: i64,
}

/// An event [`TimeSyncFollower::handle_message`] reports back after
/// successfully processing a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum TimeSyncEvent {
    /// A Sync broadcast was received; the one-way offset estimate was updated.
    Sync(TimeSync),
    /// A Delay Response completed an outstanding request; the offset was
    /// refined with a path-delay-compensated measurement.
    DelayMeasured(DelayMeasurement),
}

/// Broadcasts [`TimeSync`] messages and answers followers' Delay Requests.
#[derive(Debug, Clone, Copy, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct TimeSyncMaster {
    seq: u8,
}

impl TimeSyncMaster {
    /// Creates a new master with its Sync sequence number starting at 0.
    pub const fn new() -> Self {
        Self { seq: 0 }
    }

    /// Broadcasts `now` (this node's own clock reading, taken as close as
    /// possible to this call for the least error) as a Sync message, then
    /// increments the sequence number for next time.
    pub async fn broadcast<'ch, M, const TXN: usize, const RXN: usize>(
        &mut self,
        sender: &P2pSender<'ch, M, TXN, RXN>,
        now: u64,
    ) -> Result<(), P2pError>
    where
        M: RawMutex,
    {
        let msg = TimeSyncMessage::Sync {
            seq: self.seq,
            timestamp: now,
        };
        self.seq = self.seq.wrapping_add(1);
        let (buf, len) = msg.encode();
        sender.broadcast_timesync(&buf[..len]).await
    }

    /// Handles a message this master just received from `src`. If it's a
    /// Delay Request, replies with a Delay Response carrying `local_now`
    /// (this node's own clock reading, taken as close as possible to
    /// receipt) and returns `Ok(Some(src))`. Otherwise returns `Ok(None)`,
    /// leaving `payload` for the caller to handle as whatever else it might be.
    pub async fn handle_message<'ch, M, const TXN: usize, const RXN: usize>(
        &self,
        sender: &P2pSender<'ch, M, TXN, RXN>,
        src: PeerAddr,
        payload: &[u8],
        local_now: u64,
    ) -> Result<Option<PeerAddr>, P2pError>
    where
        M: RawMutex,
    {
        let Some(TimeSyncMessage::DelayReq { seq }) = TimeSyncMessage::decode(payload) else {
            return Ok(None);
        };
        let resp = TimeSyncMessage::DelayResp { seq, t4: local_now };
        let (buf, len) = resp.encode();
        sender.send_timesync(src, &buf[..len]).await?;
        Ok(Some(src))
    }

    /// Runs forever, broadcasting a Sync every `period_ms` using `clock` for
    /// each timestamp. Meant to be spawned as its own task (see the module
    /// docs on why sending needs no coordination with whatever task is
    /// consuming this bus's incoming p2p traffic).
    pub async fn run_broadcast<'ch, M, const TXN: usize, const RXN: usize, D, C>(
        mut self,
        sender: P2pSender<'ch, M, TXN, RXN>,
        mut delay: D,
        period_ms: u32,
        mut clock: C,
    ) -> !
    where
        M: RawMutex,
        D: DelayNs,
        C: Clock,
    {
        loop {
            let now = clock.now();
            let _ = self.broadcast(&sender, now).await;
            delay.delay_ms(period_ms).await;
        }
    }
}

/// Tracks synchronization state for a peer listening for [`TimeSyncMaster`]
/// broadcasts (and, optionally, issuing Delay Requests to refine its offset).
#[derive(Debug, Clone, Copy, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct TimeSyncFollower {
    /// `master_clock - local_clock`, refined by the last completed delay
    /// measurement if any, else the plain one-way estimate from the last Sync.
    offset: Option<i64>,
    mean_path_delay: Option<i64>,
    last_seq: Option<u8>,
    master: Option<PeerAddr>,
    /// `(t1, t2)` from the most recently received Sync, kept around to pair
    /// with a later Delay Response.
    last_sync: Option<(u64, u64)>,
    /// `(seq, t3)` for an outstanding Delay Request awaiting its response.
    pending_req: Option<(u8, u64)>,
    req_seq: u8,
}

/// Errors from [`TimeSyncFollower::request_delay`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum DelayReqError {
    /// No Sync has been received yet, so there's no known master to ask.
    NoMaster,
    /// The underlying p2p send failed.
    Send(P2pError),
}

impl From<P2pError> for DelayReqError {
    fn from(err: P2pError) -> Self {
        Self::Send(err)
    }
}

impl TimeSyncFollower {
    /// Creates a follower with no synchronization state yet.
    pub const fn new() -> Self {
        Self {
            offset: None,
            mean_path_delay: None,
            last_seq: None,
            master: None,
            last_sync: None,
            pending_req: None,
            req_seq: 0,
        }
    }

    /// Feeds a message just received over p2p (from `src`, whatever
    /// [`crate::P2pReceiver::receive`] returned) into this follower.
    ///
    /// `local_now` must be this node's own clock reading, in the same unit
    /// and epoch the master uses, taken as close as possible to when
    /// `payload` was received.
    ///
    /// Returns `Some` (having updated this follower's state) if `payload`
    /// decoded as a Sync or Delay Response; returns `None`, untouched, if it
    /// didn't — in which case the application should handle `payload` as
    /// whatever else it might be.
    pub fn handle_message(
        &mut self,
        src: PeerAddr,
        payload: &[u8],
        local_now: u64,
    ) -> Option<TimeSyncEvent> {
        match TimeSyncMessage::decode(payload)? {
            TimeSyncMessage::Sync { seq, timestamp } => {
                self.offset = Some(timestamp as i64 - local_now as i64);
                self.last_seq = Some(seq);
                self.master = Some(src);
                self.last_sync = Some((timestamp, local_now));
                Some(TimeSyncEvent::Sync(TimeSync { seq, timestamp }))
            }
            TimeSyncMessage::DelayResp { seq, t4 } => {
                if self.master != Some(src) {
                    return None; // response from someone other than our master
                }
                let (pending_seq, t3) = self.pending_req.take()?;
                if pending_seq != seq {
                    return None; // stale or mismatched response
                }
                let (t1, t2) = self.last_sync?;
                let forward = t2 as i64 - t1 as i64;
                let backward = t4 as i64 - t3 as i64;
                let offset = (backward - forward) / 2;
                let mean_path_delay = (forward + backward) / 2;
                self.offset = Some(offset);
                self.mean_path_delay = Some(mean_path_delay);
                Some(TimeSyncEvent::DelayMeasured(DelayMeasurement {
                    offset,
                    mean_path_delay,
                }))
            }
            TimeSyncMessage::DelayReq { .. } => None, // only a TimeSyncMaster answers these
        }
    }

    /// Records `now` as `t3` for a fresh Delay Request sequence number,
    /// returning `(master, seq)` to actually send it to, or `None` if no
    /// Sync has been received yet.
    ///
    /// This is the synchronous half of [`Self::request_delay`], split out
    /// so that a caller holding this follower behind a lock — see
    /// [`SharedTimeSyncFollower`] and [`request_delay_shared`] — only needs
    /// to hold it for this instant state update, `.await`ing the actual
    /// send afterwards without holding it (the blocking mutex this crate
    /// expects [`TimeSyncFollower`] to be shared behind isn't meant to be
    /// held across an `.await`).
    pub fn prepare_delay_request(&mut self, now: u64) -> Option<(PeerAddr, u8)> {
        let master = self.master?;
        let seq = self.req_seq;
        self.req_seq = self.req_seq.wrapping_add(1);
        self.pending_req = Some((seq, now));
        Some((master, seq))
    }

    /// Sends a Delay Request to the master this follower last heard a Sync
    /// from, to refine its offset with a path-delay-compensated
    /// measurement. `now` must be this node's own clock reading, taken as
    /// close as possible to this call.
    ///
    /// Returns [`DelayReqError::NoMaster`] if no Sync has been received
    /// yet. A previously outstanding, unanswered request is superseded (its
    /// eventual late response, if any, will be ignored as stale).
    pub async fn request_delay<'ch, M, const TXN: usize, const RXN: usize>(
        &mut self,
        sender: &P2pSender<'ch, M, TXN, RXN>,
        now: u64,
    ) -> Result<(), DelayReqError>
    where
        M: RawMutex,
    {
        let (master, seq) = self
            .prepare_delay_request(now)
            .ok_or(DelayReqError::NoMaster)?;
        let (buf, len) = TimeSyncMessage::DelayReq { seq }.encode();
        sender.send_timesync(master, &buf[..len]).await?;
        Ok(())
    }

    /// The peer address this follower last received a Sync from, or `None`
    /// if it hasn't received one yet.
    pub const fn master(&self) -> Option<PeerAddr> {
        self.master
    }

    /// The sequence number of the last Sync received, or `None` if it
    /// hasn't received one yet. An application that wants to notice missed
    /// broadcasts can compare successive values of this itself.
    pub const fn last_seq(&self) -> Option<u8> {
        self.last_seq
    }

    /// Returns `true` once at least one Sync has been received.
    pub const fn is_synced(&self) -> bool {
        self.offset.is_some()
    }

    /// The current best-known offset (`master_clock - local_clock`), or
    /// `None` before the first Sync is received. Refined by
    /// [`Self::request_delay`]/[`Self::handle_message`] if a Delay
    /// Request/Response has completed since the last Sync.
    pub const fn offset(&self) -> Option<i64> {
        self.offset
    }

    /// The estimated one-way bus transit time from the last completed delay
    /// measurement, or `None` if none has completed yet. See
    /// [`DelayMeasurement::mean_path_delay`].
    pub const fn mean_path_delay(&self) -> Option<i64> {
        self.mean_path_delay
    }

    /// Converts `local_now` (this node's own clock reading) into the
    /// master's clock, or `None` if no sync has been received yet.
    pub fn to_master_time(&self, local_now: u64) -> Option<u64> {
        self.offset.map(|offset| (local_now as i64 + offset) as u64)
    }
}

/// Sends a Delay Request on behalf of a [`TimeSyncFollower`] shared behind a
/// [`SharedTimeSyncFollower`] lock: locks only long enough to record the
/// outstanding request (via [`TimeSyncFollower::prepare_delay_request`]),
/// then sends without holding the lock.
///
/// `now` must be this node's own clock reading, taken as close as possible
/// to this call. Returns [`DelayReqError::NoMaster`] if `follower` hasn't
/// received a Sync yet.
pub async fn request_delay_shared<'ch, 'f, M, const TXN: usize, const RXN: usize>(
    follower: &'f SharedTimeSyncFollower<M>,
    sender: &P2pSender<'ch, M, TXN, RXN>,
    now: u64,
) -> Result<(), DelayReqError>
where
    M: RawMutex,
{
    let (master, seq) = follower
        .lock(|cell| cell.borrow_mut().prepare_delay_request(now))
        .ok_or(DelayReqError::NoMaster)?;
    let (buf, len) = TimeSyncMessage::DelayReq { seq }.encode();
    sender.send_timesync(master, &buf[..len]).await?;
    Ok(())
}

/// Runs forever, periodically sending a Delay Request on behalf of
/// `follower` every `period_ms` via [`request_delay_shared`], to keep its
/// path-delay-refined offset up to date. Meant to be spawned as its own
/// task, independent of whatever task is running a [`crate::ProtocolRunner`]
/// to actually observe the resulting `DelayResp` — see the module docs.
///
/// Silently does nothing on a given tick if `follower` has no known master
/// yet (i.e. hasn't received its first Sync) or if the send fails; there's
/// no way to report either back to the caller of a task that never returns.
pub async fn run_periodic_delay_requests<'ch, 'f, M, const TXN: usize, const RXN: usize, D, C>(
    follower: &'f SharedTimeSyncFollower<M>,
    sender: P2pSender<'ch, M, TXN, RXN>,
    mut delay: D,
    period_ms: u32,
    mut clock: C,
) -> !
where
    M: RawMutex,
    D: DelayNs,
    C: Clock,
{
    loop {
        let now = clock.now();
        let _ = request_delay_shared(follower, &sender, now).await;
        delay.delay_ms(period_ms).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Worked example (also checked by hand against the standard PTP
    /// derivation before writing this code): a follower running 50 units
    /// ahead of the master, with a symmetric 100-unit one-way bus delay in
    /// each direction.
    ///
    /// - t1 = 1000 (master sends Sync)
    /// - t2 = 1150 (follower receives Sync: 100 transit + 50 skew)
    /// - t3 = 2050 (follower sends Delay Request, 50 skew still applies)
    /// - t4 = 2100 (master receives it: 100 transit, no skew on master's own clock)
    ///
    /// Expected: offset = -50 (subtract 50 from the follower's clock to get
    /// the master's), mean_path_delay = 100 (the symmetric transit time).
    #[test]
    fn delay_exchange_recovers_skew_and_path_delay() {
        let master = PeerAddr::new(1).unwrap();
        let mut follower = TimeSyncFollower::new();

        let (buf, len) = TimeSyncMessage::Sync {
            seq: 7,
            timestamp: 1000,
        }
        .encode();
        let event = follower.handle_message(master, &buf[..len], 1150);
        assert_eq!(
            event,
            Some(TimeSyncEvent::Sync(TimeSync {
                seq: 7,
                timestamp: 1000
            }))
        );
        assert_eq!(follower.offset(), Some(1000 - 1150)); // -150: naive one-way estimate, ignores transit time
        assert_eq!(follower.master(), Some(master));

        // Simulate having sent a Delay Request at t3 = 2050 without needing
        // a live P2pSender: `request_delay` itself is a thin, separately
        // low-risk wrapper around `P2pSender::send`, so this test isolates
        // the part actually worth checking carefully — the offset/path
        // delay arithmetic in the `DelayResp` branch below.
        follower.pending_req = Some((3, 2050));

        let (buf, len) = TimeSyncMessage::DelayResp { seq: 3, t4: 2100 }.encode();
        let event = follower.handle_message(
            master,
            &buf[..len],
            /* local_now unused for DelayResp */ 0,
        );
        assert_eq!(
            event,
            Some(TimeSyncEvent::DelayMeasured(DelayMeasurement {
                offset: -50,
                mean_path_delay: 100,
            }))
        );
        assert_eq!(follower.offset(), Some(-50));
        assert_eq!(follower.mean_path_delay(), Some(100));
        assert_eq!(follower.to_master_time(2200), Some(2150)); // 2200 + (-50)

        // The outstanding request was consumed; a second, unmatched
        // response for the same seq is now stale and ignored.
        let (buf, len) = TimeSyncMessage::DelayResp { seq: 3, t4: 9999 }.encode();
        assert_eq!(follower.handle_message(master, &buf[..len], 0), None);
        assert_eq!(follower.offset(), Some(-50)); // unchanged
    }

    #[test]
    fn delay_response_from_a_different_peer_is_ignored() {
        let master = PeerAddr::new(1).unwrap();
        let impostor = PeerAddr::new(2).unwrap();
        let mut follower = TimeSyncFollower::new();

        let (buf, len) = TimeSyncMessage::Sync {
            seq: 0,
            timestamp: 1000,
        }
        .encode();
        follower.handle_message(master, &buf[..len], 1150);
        follower.pending_req = Some((0, 2050));

        let (buf, len) = TimeSyncMessage::DelayResp { seq: 0, t4: 2100 }.encode();
        assert_eq!(follower.handle_message(impostor, &buf[..len], 0), None);
        // The naive one-way estimate from the Sync is untouched.
        assert_eq!(follower.offset(), Some(1000 - 1150));
    }
}

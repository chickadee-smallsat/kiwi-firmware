//! [`ProtocolRunner`]: the sole [`P2pReceiver`] consumer for a node running
//! any combination of time synchronization (`crate::timesync`) and bus
//! arbitration (`crate::busarb`).
//!
//! Both of those protocols route their own messages over a dedicated,
//! private wire channel (see `crate::p2p`'s module docs), and — like any
//! other consumer of a [`P2pReceiver`] — need to be the *sole* thing calling
//! [`P2pReceiver::receive_raw`]. Earlier, each protocol had its own runner
//! type (`TimeSyncRunner`, `BusArbitrationRunner`) for this, which meant a
//! node had to pick exactly one: an arbiter could never also answer
//! followers' Delay Requests, since that traffic arrives on the timesync
//! channel its `BusArbitrationRunner` would silently drop, and vice versa.
//!
//! [`ProtocolRunner`] replaces both: every role (`follower`, `master`,
//! `peer`, `arbiter_role`) is independently optional, so a single node can
//! be the bus arbiter *and* answer Delay Requests *and* forward ordinary
//! P2P traffic, all from one runner — or just one of those, or none at all,
//! whatever this node actually needs.

use embassy_sync::{
    blocking_mutex::raw::RawMutex,
    channel::{Channel, Sender},
};

use crate::{
    busarb::{SharedBusArbiterRole, SharedBusArbitrationPeer},
    p2p::{P2pChannel, P2pReceiver, P2pSender, WireChannel},
    timesync::{Clock, ForwardedMessage, SharedTimeSyncFollower, TimeSyncMaster},
};

/// Owns the sole [`P2pReceiver`] consumer for a node running some
/// combination of time synchronization and bus arbitration duties — see the
/// module docs.
pub struct ProtocolRunner<
    'ch,
    'f,
    'p,
    'ar,
    'q,
    M,
    const TXN: usize,
    const RXN: usize,
    const QN: usize,
    C,
> where
    M: RawMutex,
{
    receiver: P2pReceiver<'ch, M, TXN, RXN>,
    sender: P2pSender<'ch, M, TXN, RXN>,
    follower: Option<&'f SharedTimeSyncFollower<M>>,
    master: Option<TimeSyncMaster>,
    peer: Option<&'p SharedBusArbitrationPeer<M>>,
    arbiter_role: Option<&'ar SharedBusArbiterRole<M>>,
    forward: Sender<'q, M, ForwardedMessage, QN>,
    clock: C,
}

impl<'ch, 'f, 'p, 'ar, 'q, M, const TXN: usize, const RXN: usize, const QN: usize, C>
    ProtocolRunner<'ch, 'f, 'p, 'ar, 'q, M, TXN, RXN, QN, C>
where
    M: RawMutex,
    C: Clock,
{
    /// Builds a runner around `receiver` (the p2p handle's sole consumer)
    /// and `sender` (used to answer Delay Requests/arbitration requests —
    /// not for the application's own sends, which should use their own
    /// clone of the same underlying [`P2pSender`]).
    ///
    /// Every role is independently optional — give whichever combination
    /// this node actually needs:
    /// - `follower`/`master`: time synchronization, see `crate::timesync`.
    /// - `peer`/`arbiter_role`: bus arbitration, see `crate::busarb`. `peer`
    ///   tracks this node's own view (known arbiter, directory cache,
    ///   election bookkeeping); `arbiter_role` answers requests, and is only
    ///   `Some` while this node currently holds the arbiter role itself.
    ///
    /// `forward_channel` carries [`P2pChannel::App`]/[`P2pChannel::Control`]
    /// messages this runner doesn't consume through to the application;
    /// this returns the [`embassy_sync::channel::Receiver`] half of it for
    /// the application to poll. It's borrowed for `'q`, so callers
    /// typically provide a `&'static` channel (e.g. via a `static`, as
    /// elsewhere in this workspace).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        receiver: P2pReceiver<'ch, M, TXN, RXN>,
        sender: P2pSender<'ch, M, TXN, RXN>,
        follower: Option<&'f SharedTimeSyncFollower<M>>,
        master: Option<TimeSyncMaster>,
        peer: Option<&'p SharedBusArbitrationPeer<M>>,
        arbiter_role: Option<&'ar SharedBusArbiterRole<M>>,
        forward_channel: &'q Channel<M, ForwardedMessage, QN>,
        clock: C,
    ) -> (
        Self,
        embassy_sync::channel::Receiver<'q, M, ForwardedMessage, QN>,
    ) {
        let runner = Self {
            receiver,
            sender,
            follower,
            master,
            peer,
            arbiter_role,
            forward: forward_channel.sender(),
            clock,
        };
        (runner, forward_channel.receiver())
    }

    /// Runs forever. Meant to be handed to a `#[embassy_executor::task]`
    /// function defined at the call site, the same way
    /// [`crate::Mcp25625Runner::run`] is spawned elsewhere in this workspace.
    pub async fn run(mut self) -> ! {
        // Routing is decided purely by which wire channel
        // [`P2pReceiver::receive_raw`] reports a message on, never by trying to
        // decode it and seeing whether that succeeds: the private timesync
        // channel always goes to `follower`/`master` (whichever are given, else
        // dropped) and the private arbitration channel to `peer`/`arbiter_role`
        // (likewise) — neither is ever forwarded, even if nothing consumes it.
        // [`P2pChannel::App`] and [`P2pChannel::Control`] messages always go
        // straight to `forward`, unexamined and tagged with which of the two
        // they arrived on.
        loop {
            let (src, channel, payload) = self.receiver.receive_raw().await;
            match channel {
                WireChannel::TimeSync => {
                    let now = self.clock.now();
                    let mut handled = false;
                    if let Some(follower) = self.follower {
                        handled = follower.lock(|cell| {
                            cell.borrow_mut()
                                .handle_message(src, &payload, now)
                                .is_some()
                        });
                    }
                    if !handled {
                        if let Some(master) = &self.master {
                            let _ = master
                                .handle_message(&self.sender, src, &payload, now)
                                .await;
                        }
                    }
                }
                WireChannel::Arbitration => {
                    let now = self.clock.now();
                    if let Some(peer) = self.peer {
                        let _ =
                            peer.lock(|cell| cell.borrow_mut().handle_message(src, &payload, now));
                    }
                    if let Some(arbiter_role) = self.arbiter_role {
                        let reply = arbiter_role.lock(|cell| {
                            cell.borrow_mut()
                                .as_mut()
                                .and_then(|arbiter| arbiter.handle_sync(src, &payload, now))
                        });
                        if let Some((dest, resp)) = reply {
                            let (buf, len) = resp.encode();
                            let _ = self.sender.send_arbitration(dest, &buf[..len]).await;
                        }
                    }
                }
                WireChannel::App => {
                    let _ = self.forward.try_send((src, P2pChannel::App, payload));
                }
                WireChannel::Control => {
                    let _ = self.forward.try_send((src, P2pChannel::Control, payload));
                }
            }
        }
    }
}

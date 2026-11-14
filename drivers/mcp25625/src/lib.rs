#![no_std]
#![deny(missing_docs)]
#![allow(dead_code)]
// `doc(cfg(...))` (the "Available on crate feature `x` only" badges docs.rs
// renders) needs the nightly-only `doc_cfg` feature — gated on the `docsrs`
// cfg docs.rs's nightly rustdoc sets (see `Cargo.toml`'s
// `[package.metadata.docs.rs]`) so this has no effect on a normal, stable,
// local build.
#![cfg_attr(docsrs, feature(doc_cfg))]
//! `no_std`, async Rust driver for the Microchip MCP25625 CAN 2.0B controller
//! with integrated transceiver.
//!
//! The MCP25625 shares its register map with the MCP2515 and communicates
//! over SPI. It supports classic CAN 2.0B only: 11-bit and 29-bit
//! identifiers, up to 8 data bytes, up to 1 Mbps.
//!
//! [`Mcp25625`] is the low-level driver. For use across multiple `embassy`
//! tasks, [`split`] builds a queued handle/runner pair on top of it;
//! [`IsoTpChannel`] layers ISO-TP style segmentation on top of a handle for
//! a single point-to-point conversation of up to 4095 bytes; and [`new_p2p`]
//! layers a lightweight many-to-many datagram protocol on top of a handle,
//! addressing up to [`MAX_PEERS`] peers by a one-hot bit baked into the CAN
//! ID (see the `p2p` module docs), for messages up to [`MAX_P2P_MSG_LEN`]
//! bytes each.
//! [`TimeSyncMaster`]/[`TimeSyncFollower`] layer a PTP-like clock
//! synchronization protocol on top of p2p, with an optional Delay
//! Request/Response exchange to compensate for bus transit time.
//! [`BusArbiter`]/[`BusArbitrationPeer`] layer a peer-membership,
//! role-registration, and bus-access coordination protocol on top of p2p —
//! see the `busarb` module docs. [`ProtocolRunner`] drives whichever
//! combination of the two a node needs, as the sole consumer of its
//! [`P2pReceiver`].
//!
//! Raw single-frame transmit/receive (on both [`Mcp25625`] itself and
//! [`Mcp25625Handle`]) is gated behind the `raw-can` feature, off by default.
//! Real communication should go through one of the protocols above, which
//! give you addressing, fragmentation, and (for p2p) hardware acceptance
//! filtering for free; enable `raw-can` only for bring-up/debugging or to
//! build a protocol of your own directly on frames.
//!
//! p2p and ISO-TP are deliberately independent — there's no combined type
//! that switches between them at runtime. Both ultimately require being the
//! sole consumer of a [`Mcp25625Handle`]'s incoming queue (see the `p2p` and
//! ISO-TP docs), and p2p additionally supports a long-running [`ProtocolRunner`]
//! task that needs to own that consumer for as long as it runs; a type that
//! could reassign the same handle between protocols at runtime would either
//! have to forbid that, or risk two consumers racing for the same frames.
//! Pick one protocol per handle for its lifetime, constructing whichever you
//! need directly from a [`Mcp25625Handle`]. See the crate README for the
//! overall design.
//!
//! # Quick start
//!
//! This builds a [`split`] handle/runner pair and exchanges a p2p datagram
//! over it — the recommended way to actually talk on the bus (see above for
//! why). `spawner`/`tx_channel`/`rx_channel`/`filter_signal` are elided for
//! brevity; see `examples/p2p_chat.rs` for the complete, runnable version of
//! this (and `examples/isotp_transfer.rs`, `examples/timesync.rs`,
//! `examples/combined_network.rs` for the other protocols), runnable with
//! `cargo run -p mcp25625 --no-default-features --example <name>`.
//!
//! ```rust,ignore
//! use mcp25625::{
//!     hardware_filter, new_p2p, split, BitTiming, Mcp25625, Mcp25625Config, OperatingMode,
//!     PeerAddr,
//! };
//!
//! // `spi` must be an `embedded_hal_async::spi::SpiDevice` wrapping the raw SPI
//! // peripheral together with the CS pin (e.g. an `embedded_hal_bus` device);
//! // `rst`/`irq` are the RST and INT GPIOs; `delay` any `embedded_hal_async::delay::DelayNs`.
//! let bit_timing = BitTiming::from_bit_rate(16_000_000, 500_000)
//!     .expect("500 kbps is achievable at 16 MHz");
//! let config = Mcp25625Config::new(bit_timing).with_mode(OperatingMode::Normal);
//! let mut can = Mcp25625::new(spi, rst, irq, delay, config);
//!
//! let local = PeerAddr::Peer0;
//! can.set_filter(hardware_filter(local)); // before init(): filters are only written then
//! can.init().await?;
//!
//! // `tx_channel`/`rx_channel` are `embassy_sync::channel::Channel<M, CanFrame, N>`;
//! // `filter_signal` is an `embassy_sync::signal::Signal<M, RxFilter>` (for
//! // `Mcp25625Handle::update_filter` — see that method). All three are
//! // typically `&'static` (e.g. via `static_cell::StaticCell`).
//! let (handle, runner) = split(can, tx_channel, rx_channel, filter_signal);
//! spawner.spawn(can_runner_task(runner))?; // a task that just calls `runner.run()`
//!
//! let (sender, mut receiver) = new_p2p(handle, local);
//! sender.broadcast(b"hello").await?;
//! let (from, channel, payload) = receiver.receive().await;
//! ```
//!
//! For bring-up/debugging with raw single-frame [`Mcp25625::transmit`]/
//! [`Mcp25625::receive`] instead, enable the `raw-can` feature — see above
//! for why real communication should normally go through one of the
//! protocols instead.

mod bus;
mod busarb;
mod config;
mod driver;
mod frame;
mod p2p;
mod protocol_runner;
mod registers;
mod runner;
mod timesync;
mod transport;

pub use busarb::{
    bootstrap, run_arbiter_supervisor, BusArbiter, BusArbitrationEvent, BusArbitrationPeer,
    RegistrationOutcome, Role, SharedBusArbiterRole, SharedBusArbitrationPeer, ARBITER_ADDR,
    ASSIGNABLE_PEERS, UNASSIGNED_ADDR,
};
pub use config::{BitTiming, Mcp25625Config, OperatingMode, RxFilter};
pub use driver::Mcp25625;
pub use frame::{CanFrame, ExtendedId, Id, StandardId, MAX_DATA_LEN};
pub use p2p::{
    hardware_filter, new_p2p, Destination, P2pChannel, P2pError, P2pReceiver, P2pSender, PeerAddr,
    MAX_CONTROL_MSG_LEN, MAX_P2P_MSG_LEN, MAX_PEERS,
};
pub use protocol_runner::ProtocolRunner;
pub use registers::{Eflg, OpMode};
pub use runner::{split, Mcp25625Handle, Mcp25625Runner};
pub use timesync::{
    request_delay_shared, run_periodic_delay_requests, Clock, DelayMeasurement, DelayReqError,
    ForwardedMessage, SharedTimeSyncFollower, TimeSync, TimeSyncEvent, TimeSyncFollower,
    TimeSyncMaster,
};
pub use transport::{IsoTpChannel, IsoTpConfig, IsoTpError, MAX_ISO_TP_LEN};

/// Errors that can occur when interacting with the MCP25625.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Mcp25625Error<CommError> {
    /// SPI communication error.
    Comm(CommError),
    /// The device did not report Configuration mode immediately after reset;
    /// SPI communication is likely not actually working.
    InvalidDevice,
    /// A requested mode change was not confirmed by the device in time.
    ModeTimeout,
    /// All TX buffers remained occupied for the polling budget.
    TxBusy,
    /// A bus error was observed while waiting to receive; the latched EFLG
    /// contents are included for diagnostics.
    Bus(Eflg),
}

impl<CommError> From<CommError> for Mcp25625Error<CommError> {
    fn from(err: CommError) -> Self {
        Mcp25625Error::Comm(err)
    }
}

#[cfg(feature = "defmt-messages")]
#[allow(unused_imports)]
use defmt::{debug, error, info, trace, warn};

// A replacement for the defmt logging macros, when defmt is not provided
#[cfg(not(feature = "defmt-messages"))]
#[allow(unused_imports)]
mod log_impl {
    #![allow(unused_macros)]
    macro_rules! _trace {
        ($($arg:tt)*) => {};
    }
    macro_rules! _debug {
        ($($arg:tt)*) => {};
    }
    macro_rules! _info {
        ($($arg:tt)*) => {};
    }
    macro_rules! _warn {
        ($($arg:tt)*) => {};
    }
    macro_rules! _error {
        ($($arg:tt)*) => {};
    }
    pub(crate) use _debug as debug;
    pub(crate) use _error as error;
    pub(crate) use _info as info;
    pub(crate) use _trace as trace;
    pub(crate) use _warn as warn;
}
#[cfg(not(feature = "defmt-messages"))]
#[allow(unused_imports)]
use log_impl::*;

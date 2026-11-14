//! Queued, multi-task-friendly front end built on top of the raw [`Mcp25625`] driver.
//!
//! This mirrors the driver/runner split used by `cyw43` and `embassy-net`
//! elsewhere in this workspace: [`split`] hands back a cheaply-clonable
//! [`Mcp25625Handle`] for application tasks to submit/consume frames, and a
//! [`Mcp25625Runner`] whose `run()` method should be spawned as an
//! `embassy-executor` task where the concrete SPI/pin/delay types are known.
//! This crate itself stays executor-agnostic; only `embassy-sync` channels
//! and `embassy-futures::select` are needed to implement the queueing.

use core::convert::Infallible;

use embassy_futures::select::{select3, Either3};
use embassy_sync::{
    blocking_mutex::raw::RawMutex,
    channel::{Channel, Receiver, Sender},
    signal::Signal,
};
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal_async::{delay::DelayNs, digital::Wait, spi::SpiDevice};

use crate::{config::RxFilter, driver::Mcp25625, frame::CanFrame};

/// Cheaply clonable handle for submitting frames to transmit and awaiting
/// received frames. Backed by the queues serviced by a [`Mcp25625Runner`].
pub struct Mcp25625Handle<'ch, M: RawMutex, const TXN: usize, const RXN: usize> {
    tx: Sender<'ch, M, CanFrame, TXN>,
    rx: Receiver<'ch, M, CanFrame, RXN>,
    filter: &'ch Signal<M, RxFilter>,
}

impl<M: RawMutex, const TXN: usize, const RXN: usize> Clone for Mcp25625Handle<'_, M, TXN, RXN> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx,
            rx: self.rx,
            filter: self.filter,
        }
    }
}

impl<M: RawMutex, const TXN: usize, const RXN: usize> Mcp25625Handle<'_, M, TXN, RXN> {
    /// Requests that the [`Mcp25625Runner`] servicing this handle apply
    /// `filter` immediately, via [`Mcp25625::update_filter`] — without a
    /// full reset, but still briefly halting receive/transmit for the mode
    /// switch that requires (see that method's docs).
    ///
    /// Only the most recently requested filter matters if this is called
    /// again before the runner gets to act on the first one — there's
    /// nothing to queue, unlike frames.
    pub fn update_filter(&self, filter: RxFilter) {
        self.filter.signal(filter);
    }
}

impl<'ch, M: RawMutex, const TXN: usize, const RXN: usize> Mcp25625Handle<'ch, M, TXN, RXN> {
    /// Queues `frame` for transmission, waiting if the outgoing queue is full.
    ///
    /// Crate-internal building block for the higher-level protocols ([`crate::new_p2p`],
    /// [`crate::IsoTpChannel`]); see [`Mcp25625Handle::transmit`] for the
    /// public, `raw-can` feature-gated equivalent.
    pub(crate) async fn transmit_impl(&self, frame: CanFrame) {
        self.tx.send(frame).await
    }

    /// Waits for the next received frame.
    ///
    /// Crate-internal building block for the higher-level protocols ([`crate::new_p2p`],
    /// [`crate::IsoTpChannel`]); see [`Mcp25625Handle::receive`] for the
    /// public, `raw-can` feature-gated equivalent.
    pub(crate) async fn receive_impl(&self) -> CanFrame {
        self.rx.receive().await
    }
}

/// Raw single-frame transmit/receive on a queued handle, gated behind the
/// `raw-can` feature.
///
/// These bypass every peer/addressing/fragmentation concept the rest of this
/// crate provides: prefer [`crate::new_p2p`] (with [`crate::BusArbiter`]) or
/// [`crate::IsoTpChannel`] for actual communication. Mixing raw sends/receives
/// on a handle with either of those is unsupported — both assume they are the
/// sole consumer of the handle's incoming queue (see their module docs).
#[cfg(feature = "raw-can")]
#[cfg_attr(docsrs, doc(cfg(feature = "raw-can")))]
impl<M: RawMutex, const TXN: usize, const RXN: usize> Mcp25625Handle<'_, M, TXN, RXN> {
    /// Queues `frame` for transmission, waiting if the outgoing queue is full.
    pub async fn transmit(&self, frame: CanFrame) {
        self.transmit_impl(frame).await
    }

    /// Waits for the next received frame.
    pub async fn receive(&self) -> CanFrame {
        self.receive_impl().await
    }
}

/// Owns the MCP25625 peripheral and services the transmit/receive queues.
///
/// `run()` loops forever, forwarding queued outgoing frames to the bus and
/// incoming bus frames to the RX queue, and applying any filter update
/// requested via [`Mcp25625Handle::update_filter`]. It does *not* initialize
/// the device itself — call [`Mcp25625::init`] on the driver passed to
/// [`split`] before spawning the runner (every call site in this crate's
/// examples and firmware does this already, typically in a retry loop, and
/// often to apply an acceptance filter via `set_filter` beforehand too).
/// Calling `init()` again here would be a second, redundant hardware/soft
/// reset immediately after the first one already succeeded, which can
/// itself fail against real hardware.
/// A frame that arrives while the RX queue is full is dropped rather than
/// blocking outgoing transmission; size `RXN` for the expected consumer
/// latency rather than relying on backpressure here.
pub struct Mcp25625Runner<'ch, SPI, RST, IRQ, D, M: RawMutex, const TXN: usize, const RXN: usize> {
    driver: Mcp25625<SPI, RST, IRQ, D>,
    tx: Receiver<'ch, M, CanFrame, TXN>,
    rx: Sender<'ch, M, CanFrame, RXN>,
    filter: &'ch Signal<M, RxFilter>,
}

impl<'ch, SPI, RST, IRQ, D, E, M, const TXN: usize, const RXN: usize>
    Mcp25625Runner<'ch, SPI, RST, IRQ, D, M, TXN, RXN>
where
    SPI: SpiDevice<Error = E>,
    RST: OutputPin<Error = Infallible>,
    IRQ: InputPin<Error = Infallible> + Wait<Error = Infallible>,
    D: DelayNs,
    M: RawMutex,
{
    /// Services the TX/RX queues (and filter update requests) forever.
    /// Assumes the driver handed to [`split`] was already initialized (see
    /// this struct's docs) — does not call [`Mcp25625::init`] itself.
    ///
    /// Meant to be handed to a `#[embassy_executor::task]` function defined
    /// at the call site (where the concrete peripheral types are known), the
    /// same way `cyw43::Runner::run()` and `embassy_net::Runner::run()` are
    /// spawned in this workspace's `wifi.rs`.
    pub async fn run(mut self) -> ! {
        loop {
            match select3(
                self.tx.receive(),
                self.driver.receive_impl(),
                self.filter.wait(),
            )
            .await
            {
                Either3::First(frame) => {
                    let _ = self.driver.transmit_impl(&frame).await;
                }
                Either3::Second(Ok(frame)) => {
                    #[cfg(feature = "defmt-messages")]
                    if self.rx.try_send(frame).is_err() {
                        defmt::warn!("MCP25625: RX queue full, dropping frame");
                    }
                    #[cfg(not(feature = "defmt-messages"))]
                    let _ = self.rx.try_send(frame);
                }
                Either3::Second(Err(_e)) => {
                    #[cfg(feature = "defmt-messages")]
                    defmt::warn!("MCP25625: bus error observed while waiting to receive");
                }
                Either3::Third(filter) => {
                    #[cfg(feature = "defmt-messages")]
                    if self.driver.update_filter(filter).await.is_err() {
                        defmt::warn!("MCP25625: failed to apply updated filter");
                    }
                    #[cfg(not(feature = "defmt-messages"))]
                    let _ = self.driver.update_filter(filter).await;
                }
            }
        }
    }
}

/// Splits a raw [`Mcp25625`] driver into a queued [`Mcp25625Handle`] (for
/// callers that submit/consume frames) and a [`Mcp25625Runner`] (to be
/// spawned as a task and driven with `run()`).
///
/// `tx_channel` carries outgoing frames from the handle to the runner;
/// `rx_channel` carries incoming frames from the runner to the handle;
/// `filter_signal` carries [`Mcp25625Handle::update_filter`] requests the
/// same way. All three are borrowed for `'ch`, so callers typically provide
/// `&'static` values (e.g. via a `static`, as elsewhere in this workspace).
#[allow(clippy::type_complexity)]
pub fn split<'ch, SPI, RST, IRQ, D, M, const TXN: usize, const RXN: usize>(
    driver: Mcp25625<SPI, RST, IRQ, D>,
    tx_channel: &'ch Channel<M, CanFrame, TXN>,
    rx_channel: &'ch Channel<M, CanFrame, RXN>,
    filter_signal: &'ch Signal<M, RxFilter>,
) -> (
    Mcp25625Handle<'ch, M, TXN, RXN>,
    Mcp25625Runner<'ch, SPI, RST, IRQ, D, M, TXN, RXN>,
)
where
    M: RawMutex,
{
    let handle = Mcp25625Handle {
        tx: tx_channel.sender(),
        rx: rx_channel.receiver(),
        filter: filter_signal,
    };
    let runner = Mcp25625Runner {
        driver,
        tx: tx_channel.receiver(),
        rx: rx_channel.sender(),
        filter: filter_signal,
    };
    (handle, runner)
}

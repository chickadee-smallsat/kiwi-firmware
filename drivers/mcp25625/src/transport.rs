//! ISO-TP (ISO 15765-2) style segmentation and reassembly on top of the raw
//! 8-byte [`CanFrame`], allowing a single message of up to
//! [`MAX_ISO_TP_LEN`] bytes to be sent or received as one unit even though
//! the MCP25625 itself only ever puts 8 bytes on the wire per frame.
//!
//! This builds on [`Mcp25625Handle`] rather than the raw driver, so it
//! inherits that queued, task-friendly front end. An [`IsoTpChannel`] takes
//! ownership of its handle and is meant to be the *sole* consumer of that
//! handle's `receive()` queue: it is a point-to-point session, not a
//! multiplexer. If several peers' traffic (distinguished by CAN ID) shares
//! one handle, only frames matching this channel's configured `rx_id` are
//! reassembled — everything else is silently ignored. Running multiple
//! concurrent `IsoTpChannel`s over *cloned* handles would race for the same
//! underlying queue (`embassy-sync` channels are competing-consumer, not
//! broadcast); use one channel per physical conversation.

use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_hal_async::delay::DelayNs;
use heapless::Vec;

use crate::{
    frame::{CanFrame, Id, StandardId, MAX_DATA_LEN},
    runner::Mcp25625Handle,
};

const PCI_SF: u8 = 0x0;
const PCI_FF: u8 = 0x1;
const PCI_CF: u8 = 0x2;
const PCI_FC: u8 = 0x3;

const FC_CONTINUE: u8 = 0x0;
const FC_WAIT: u8 = 0x1;
const FC_OVERFLOW: u8 = 0x2;

/// Largest payload addressable by the 12-bit ISO-TP First-Frame length
/// field. `IsoTpChannel`'s own `MAX_LEN` reassembly buffer may be smaller.
pub const MAX_ISO_TP_LEN: usize = 4095;

/// Errors from an ISO-TP send or receive operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum IsoTpError {
    /// The payload is larger than [`MAX_ISO_TP_LEN`] or larger than this
    /// channel's `MAX_LEN` reassembly buffer.
    TooLarge,
    /// The peer reported Flow Control overflow in response to our First Frame.
    PeerOverflow,
    /// No Flow Control frame arrived within the configured timeout.
    FlowControlTimeout,
    /// No Consecutive Frame arrived within the configured timeout while reassembling.
    ConsecutiveFrameTimeout,
    /// A Consecutive Frame arrived with an unexpected sequence number.
    SequenceError,
    /// The peer's First Frame declared a length larger than this channel's `MAX_LEN` buffer.
    ReceiveOverflow,
}

/// Configuration for an [`IsoTpChannel`].
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct IsoTpConfig {
    /// CAN identifier this channel transmits Single/First/Consecutive/Flow-Control frames on.
    pub tx_id: Id,
    /// CAN identifier this channel expects the peer's frames on; used to
    /// filter the (potentially shared) incoming frame stream.
    pub rx_id: Id,
    /// Block size advertised in this channel's own Flow Control frames: the
    /// number of Consecutive Frames a sender may transmit before waiting for
    /// another Flow Control from us. `0` means unlimited.
    pub block_size: u8,
    /// STmin advertised in this channel's own Flow Control frames, raw
    /// ISO-TP encoding (`0x00..=0x7F` = milliseconds, `0xF1..=0xF9` =
    /// 100..900 microseconds, anything else reserved/treated as `0`).
    pub st_min: u8,
    /// Timeout, in milliseconds, for a peer's Flow Control response (after we
    /// send a First Frame) or its next Consecutive Frame (while we
    /// reassemble an incoming message).
    pub timeout_ms: u32,
}

impl Default for IsoTpConfig {
    /// `tx_id`/`rx_id` default to CAN ID 0, which is almost never what you
    /// want for a real conversation — set them explicitly.
    fn default() -> Self {
        Self {
            tx_id: Id::Standard(StandardId::ZERO),
            rx_id: Id::Standard(StandardId::ZERO),
            block_size: 0,
            st_min: 0,
            timeout_ms: 1000,
        }
    }
}

/// A point-to-point ISO-TP session layered on a [`Mcp25625Handle`],
/// supporting payloads up to `MAX_LEN` bytes (and at most
/// [`MAX_ISO_TP_LEN`]).
///
/// `MAX_LEN` sizes an internal `[u8; MAX_LEN]` reassembly scratch buffer that
/// lives in `receive()`'s async state, so pick it no larger than the message
/// sizes you actually expect — this crate has no allocator to fall back on.
pub struct IsoTpChannel<'ch, M, const TXN: usize, const RXN: usize, D, const MAX_LEN: usize>
where
    M: RawMutex,
{
    can: Mcp25625Handle<'ch, M, TXN, RXN>,
    delay: D,
    config: IsoTpConfig,
}

impl<'ch, M, const TXN: usize, const RXN: usize, D, const MAX_LEN: usize>
    IsoTpChannel<'ch, M, TXN, RXN, D, MAX_LEN>
where
    M: RawMutex,
    D: DelayNs,
{
    /// Creates a new channel, taking ownership of `can` (see the module docs
    /// on why this channel must be the sole consumer of its handle's
    /// `receive()` queue).
    pub fn new(can: Mcp25625Handle<'ch, M, TXN, RXN>, delay: D, config: IsoTpConfig) -> Self {
        Self { can, delay, config }
    }

    /// Sends `data` as a single ISO-TP message, transparently split across a
    /// Single Frame or a First Frame followed by Consecutive Frames as
    /// needed. Waits for the peer's Flow Control after the First Frame (and
    /// again every `block_size` Consecutive Frames the peer advertises),
    /// pacing subsequent frames by the peer's advertised STmin.
    pub async fn send(&mut self, data: &[u8]) -> Result<(), IsoTpError> {
        if data.len() > MAX_ISO_TP_LEN || data.len() > MAX_LEN {
            return Err(IsoTpError::TooLarge);
        }

        if data.len() < MAX_DATA_LEN {
            // Single Frame: PCI high nibble 0, low nibble = length (0..=7).
            let mut buf = [0u8; MAX_DATA_LEN];
            buf[0] = (PCI_SF << 4) | (data.len() as u8);
            buf[1..1 + data.len()].copy_from_slice(data);
            self.transmit(&buf[..1 + data.len()]).await;
            return Ok(());
        }

        // First Frame: 12-bit length across the low nibble of byte 0 and byte 1,
        // followed by the first 6 data bytes.
        let len = data.len() as u16;
        let mut ff = [0u8; MAX_DATA_LEN];
        ff[0] = (PCI_FF << 4) | ((len >> 8) as u8 & 0x0F);
        ff[1] = (len & 0xFF) as u8;
        let first_chunk = MAX_DATA_LEN - 2;
        ff[2..].copy_from_slice(&data[..first_chunk]);
        self.transmit(&ff).await;

        let (mut peer_bs, mut peer_st_min) = self.await_flow_control().await?;
        let mut sent = first_chunk;
        let mut seq: u8 = 1;
        let mut in_block: u8 = 0;
        while sent < data.len() {
            let chunk_len = (data.len() - sent).min(MAX_DATA_LEN - 1);
            let mut cf = [0u8; MAX_DATA_LEN];
            cf[0] = (PCI_CF << 4) | (seq & 0x0F);
            cf[1..1 + chunk_len].copy_from_slice(&data[sent..sent + chunk_len]);
            self.transmit(&cf[..1 + chunk_len]).await;
            sent += chunk_len;
            seq = seq.wrapping_add(1);
            in_block += 1;

            if sent < data.len() {
                if peer_bs != 0 && in_block >= peer_bs {
                    let (bs, st_min) = self.await_flow_control().await?;
                    peer_bs = bs;
                    peer_st_min = st_min;
                    in_block = 0;
                } else {
                    self.delay.delay_us(st_min_to_us(peer_st_min)).await;
                }
            }
        }
        Ok(())
    }

    /// Waits for and reassembles the next ISO-TP message addressed to
    /// `rx_id`. Frames on other CAN IDs, and stray Consecutive/Flow-Control
    /// frames outside of an active reassembly, are silently ignored.
    pub async fn receive(&mut self) -> Result<Vec<u8, MAX_LEN>, IsoTpError> {
        loop {
            let frame = self.wait_matching_frame().await;
            let data = frame.data();
            if data.is_empty() {
                continue;
            }
            match data[0] >> 4 {
                pci if pci == PCI_SF => {
                    let len = (data[0] & 0x0F) as usize;
                    if len == 0 || len > data.len() - 1 {
                        continue; // malformed; keep waiting for a valid message
                    }
                    return Vec::from_slice(&data[1..1 + len]).map_err(|_| IsoTpError::TooLarge);
                }
                pci if pci == PCI_FF => {
                    if data.len() < 2 {
                        continue;
                    }
                    let len = (((data[0] & 0x0F) as usize) << 8) | data[1] as usize;
                    if len > MAX_LEN {
                        self.send_flow_control(FC_OVERFLOW).await;
                        return Err(IsoTpError::ReceiveOverflow);
                    }
                    let mut buf = [0u8; MAX_LEN];
                    let first_chunk = (data.len() - 2).min(len);
                    buf[..first_chunk].copy_from_slice(&data[2..2 + first_chunk]);
                    let mut received = first_chunk;
                    self.send_flow_control(FC_CONTINUE).await;

                    let mut expected_seq: u8 = 1;
                    let mut in_block: u8 = 0;
                    while received < len {
                        let cf = self.await_consecutive_frame().await?;
                        let cdata = cf.data();
                        if cdata.is_empty() || cdata[0] >> 4 != PCI_CF {
                            return Err(IsoTpError::SequenceError);
                        }
                        if cdata[0] & 0x0F != expected_seq & 0x0F {
                            return Err(IsoTpError::SequenceError);
                        }
                        let chunk = (cdata.len() - 1).min(len - received);
                        buf[received..received + chunk].copy_from_slice(&cdata[1..1 + chunk]);
                        received += chunk;
                        expected_seq = expected_seq.wrapping_add(1);
                        in_block += 1;

                        if received < len
                            && self.config.block_size != 0
                            && in_block >= self.config.block_size
                        {
                            self.send_flow_control(FC_CONTINUE).await;
                            in_block = 0;
                        }
                    }
                    return Vec::from_slice(&buf[..len]).map_err(|_| IsoTpError::TooLarge);
                }
                _ => continue, // stray CF/FC with no First Frame in progress
            }
        }
    }

    async fn transmit(&self, bytes: &[u8]) {
        let frame = CanFrame::new(self.config.tx_id, bytes)
            .expect("ISO-TP frames are always built within the 8-byte classic CAN payload");
        self.can.transmit_impl(frame).await;
    }

    async fn send_flow_control(&self, status: u8) {
        let buf = [
            (PCI_FC << 4) | status,
            self.config.block_size,
            self.config.st_min,
        ];
        self.transmit(&buf).await;
    }

    /// Awaits the next frame on `rx_id`, ignoring everything else. Has no
    /// timeout of its own; callers race it against one where needed.
    ///
    /// Deliberately takes `&self.can` and `rx_id` rather than being a
    /// `&self` method: [`Self::await_consecutive_frame`] and
    /// [`Self::await_flow_control`] need to hold this borrow alongside a
    /// simultaneous `&mut self.delay` inside `select`, which a whole-`&self`
    /// method call would prevent.
    async fn wait_matching_frame(&self) -> CanFrame {
        wait_on(&self.can, self.config.rx_id).await
    }

    async fn await_consecutive_frame(&mut self) -> Result<CanFrame, IsoTpError> {
        let timeout_ms = self.config.timeout_ms;
        let rx_id = self.config.rx_id;
        match select(wait_on(&self.can, rx_id), self.delay.delay_ms(timeout_ms)).await {
            Either::First(frame) => Ok(frame),
            Either::Second(_) => Err(IsoTpError::ConsecutiveFrameTimeout),
        }
    }

    /// Waits for a Flow Control frame on `rx_id`, returning the peer's
    /// advertised `(block_size, st_min)`. A `WAIT` status keeps this loop
    /// waiting (within the same overall timeout window rather than a fresh
    /// one per `WAIT`, unlike a strict ISO 15765-2 N_Bs timer restart); an
    /// `OVERFLOW` status resolves to [`IsoTpError::PeerOverflow`].
    async fn await_flow_control(&mut self) -> Result<(u8, u8), IsoTpError> {
        let timeout_ms = self.config.timeout_ms;
        let rx_id = self.config.rx_id;
        let can = &self.can;
        let wait = async {
            loop {
                let frame = wait_on(can, rx_id).await;
                let data = frame.data();
                if data.is_empty() || data[0] >> 4 != PCI_FC {
                    continue;
                }
                match data[0] & 0x0F {
                    FC_CONTINUE => {
                        let bs = data.get(1).copied().unwrap_or(0);
                        let st_min = data.get(2).copied().unwrap_or(0);
                        return Ok((bs, st_min));
                    }
                    FC_WAIT => continue,
                    FC_OVERFLOW => return Err(IsoTpError::PeerOverflow),
                    _ => continue,
                }
            }
        };
        match select(wait, self.delay.delay_ms(timeout_ms)).await {
            Either::First(result) => result,
            Either::Second(_) => Err(IsoTpError::FlowControlTimeout),
        }
    }
}

/// Awaits the next frame whose identifier is `rx_id`, ignoring everything
/// else. A free function (rather than a method borrowing all of
/// `IsoTpChannel`) so callers can hold this borrow of just the `can` field
/// alongside an independent `&mut` borrow of the channel's `delay`.
async fn wait_on<'ch, M, const TXN: usize, const RXN: usize>(
    can: &Mcp25625Handle<'ch, M, TXN, RXN>,
    rx_id: Id,
) -> CanFrame
where
    M: RawMutex,
{
    loop {
        let frame = can.receive_impl().await;
        if frame.id() == rx_id {
            return frame;
        }
    }
}

/// Converts a raw ISO-TP STmin byte into a delay in microseconds.
fn st_min_to_us(raw: u8) -> u32 {
    match raw {
        0x00..=0x7F => raw as u32 * 1000,
        0xF1..=0xF9 => (raw as u32 - 0xF0) * 100,
        _ => 0,
    }
}

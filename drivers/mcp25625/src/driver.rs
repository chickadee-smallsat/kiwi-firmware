//! Core MCP25625 driver: reset, configuration, and single-shot transmit/receive.

use core::convert::Infallible;

use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal_async::{delay::DelayNs, digital::Wait, spi::SpiDevice};

#[cfg(feature = "defmt-messages")]
use crate::registers::{TxbCtrl, TXB_CTRL};
use crate::{
    bus::Bus,
    config::{Mcp25625Config, RxFilter},
    debug,
    frame::{decode_id, encode_id, CanFrame},
    registers::{
        CanCtrl, CanIntE, CanIntF, CanStat, Cnf1, Cnf2, Cnf3, Eflg, OpMode, Rxb0Ctrl, Rxb1Ctrl,
        REG_CANCTRL, REG_CANINTE, REG_CANINTF, REG_CANSTAT, REG_CNF1, REG_CNF2, REG_CNF3, REG_EFLG,
        REG_REC, REG_RXF_SIDH, REG_RXM_SIDH, REG_TEC, RXB_CTRL, RXM_ANY, RXM_FILTERED,
    },
    trace, warn, Mcp25625Error,
};

/// Iteration budget for polling loops (mode changes, free-TX-buffer search).
/// Bounds worst-case latency instead of spinning forever on a wedged device.
const MAX_LOOPS: u32 = 2000;
/// Mask covering all bits of the 4 ID bytes (SIDH, SIDL, EID8, EID0) that
/// [`crate::frame::encode_id`] actually populates, for exact-match filtering:
///
/// - `SIDH` (`0xFF`): all 8 bits are `SID[10:3]`.
/// - `SIDL` (`0xE3` = `0b1110_0011`): bits 7:5 are `SID[2:0]` and bits 1:0 are
///   `EID[17:16]`; bits 4, 3, 2 are left clear because on a *mask* register
///   (RXMnSIDL) those bit positions are unimplemented — acceptance filtering
///   always compares standard-vs-extended via the frame's actual IDE bit
///   regardless of the mask, so there is nothing to mask there.
/// - `EID8`/`EID0` (`0xFF` each): all 8 bits are `EID[15:8]`/`EID[7:0]`.
const ID_MASK_ALL: [u8; 4] = [0xFF, 0xE3, 0xFF, 0xFF];

/// Async MCP25625 driver, owning the SPI device, reset pin, interrupt pin,
/// and a delay provider.
///
/// Typical lifecycle:
///
/// 1. [`Mcp25625::new`] — construct with a [`Mcp25625Config`] describing bit
///    timing, final operating mode, and acceptance filtering. No SPI traffic
///    happens yet.
/// 2. [`Mcp25625::init`] — hardware- and soft-reset the device, push the
///    configuration down (bit timing, interrupt enables, filters/masks), and
///    switch to the configured operating mode. Must succeed before
///    transmitting or receiving.
/// 3. Send and receive single frames, typically in a loop — see
///    [`crate::split`] for a queued version of exactly that loop, servicing
///    both directions concurrently. Raw single-frame transmit/receive on this
///    type itself is gated behind the `raw-can` feature (off by default):
///    prefer the [`crate::new_p2p`] (with [`crate::BusArbiter`]) or
///    [`crate::IsoTpChannel`] protocols built on top of [`crate::split`]'s
///    handle for actual communication, and reserve raw access for bring-up,
///    debugging, or a custom protocol of your own.
/// 4. [`Mcp25625::destroy`] — tear down and reclaim the SPI device and pins,
///    e.g. to repurpose them, if the driver is no longer needed.
///
/// The interrupt (INT) pin is not optional: receiving is purely
/// interrupt-driven (there is no register-polling fallback), so an input pin
/// capable of both synchronous level reads and asynchronously waiting for an
/// edge is required to construct a driver at all.
///
/// This is designed for a single owner (matching a dedicated SPI peripheral
/// and IRQ line, as in this workspace's CAN resource set) rather than for
/// concurrent access from independently scheduled tasks. See [`crate::split`]
/// for a queued, multi-task-friendly front end built on top of this type.
pub struct Mcp25625<SPI, RST, IRQ, D> {
    bus: Bus<SPI>,
    rst: RST,
    irq: IRQ,
    delay: D,
    config: Mcp25625Config,
}

impl<SPI, RST, IRQ, D, E> Mcp25625<SPI, RST, IRQ, D>
where
    SPI: SpiDevice<Error = E>,
    RST: OutputPin<Error = Infallible>,
    IRQ: InputPin<Error = Infallible> + Wait<Error = Infallible>,
    D: DelayNs,
{
    /// Creates a new driver instance. Call [`Mcp25625::init`] before using it.
    pub fn new(spi: SPI, rst: RST, irq: IRQ, delay: D, config: Mcp25625Config) -> Self {
        Self {
            bus: Bus { spi },
            rst,
            irq,
            delay,
            config,
        }
    }

    /// Returns the active configuration.
    pub fn config(&self) -> &Mcp25625Config {
        &self.config
    }

    /// Replaces the receive acceptance filter. Only takes effect on the
    /// next [`Mcp25625::init`] call, since filters are written to the
    /// device solely during initialization. Use [`Mcp25625::update_filter`]
    /// instead to apply a new filter immediately, without a full reset.
    pub fn set_filter(&mut self, filter: RxFilter) {
        self.config.filter = filter;
    }

    /// Applies `filter` to the device immediately, without a full
    /// [`Mcp25625::init`]: the MCP25625 only requires Configuration mode to
    /// write its RXFn/RXMn registers (unlike a full reset), so this briefly
    /// switches there, writes the new filter, and switches back to whatever
    /// operating mode was active — leaving bit timing and interrupt
    /// configuration untouched.
    ///
    /// Receiving (and any in-flight transmit) briefly halts during the mode
    /// switch, since Configuration mode can't participate on the bus — that
    /// part is an unavoidable hardware constraint, not something this driver
    /// could work around. Only meaningful after [`Mcp25625::init`] has
    /// already succeeded once.
    pub async fn update_filter(&mut self, filter: RxFilter) -> Result<(), Mcp25625Error<E>> {
        self.config.filter = filter;
        let mode = self.config.mode;
        self.set_mode(OpMode::Configuration).await?;
        self.write_filters().await?;
        self.set_mode(mode).await?;
        Ok(())
    }

    /// Tears down the driver, returning the underlying peripherals.
    pub fn destroy(self) -> (SPI, RST, IRQ, D) {
        (self.bus.spi, self.rst, self.irq, self.delay)
    }

    /// Hardware- and soft-resets the device, applies bit timing, interrupt,
    /// and filter/mask configuration, then switches to the configured final
    /// operating mode.
    pub async fn init(&mut self) -> Result<(), Mcp25625Error<E>> {
        // Hardware reset pulse (RST is active low).
        let _ = self.rst.set_low();
        self.delay.delay_us(10).await;
        let _ = self.rst.set_high();
        // Oscillator start-up plus internal reset time.
        self.delay.delay_ms(2).await;

        // Belt-and-suspenders software reset over SPI; also re-synchronizes
        // the SPI framing after the hardware reset pulse.
        self.bus.reset().await?;
        self.delay.delay_ms(2).await;

        // The device must be in Configuration mode immediately after reset;
        // failing this indicates SPI communication is not actually working.
        let stat = CanStat::from_bits(self.bus.read_register(REG_CANSTAT).await?);
        if stat.opmod() != OpMode::Configuration {
            warn!("MCP25625: not in Configuration mode after reset, SPI link is not working");
            return Err(Mcp25625Error::InvalidDevice);
        }
        debug!("MCP25625: reset complete, device in Configuration mode");

        self.write_bit_timing().await?;
        self.write_interrupt_enables().await?;
        self.write_filters().await?;

        self.set_mode(self.config.mode).await?;
        debug!("MCP25625: initialization complete");
        Ok(())
    }

    /// Writes CNF1/CNF2/CNF3 from `self.config.bit_timing`. Only valid in
    /// Configuration mode (enforced by [`Mcp25625::init`] calling this right
    /// after reset, before any mode switch).
    async fn write_bit_timing(&mut self) -> Result<(), Mcp25625Error<E>> {
        let bt = &self.config.bit_timing;
        let cnf1 = Cnf1::new().with_brp(bt.brp).with_sjw(bt.sjw - 1);
        let cnf2 = Cnf2::new()
            .with_prseg(bt.prop_seg - 1)
            .with_phseg1(bt.phase_seg1 - 1)
            .with_sam(bt.triple_sample)
            .with_btlmode(true);
        let cnf3 = Cnf3::new()
            .with_phseg2(bt.phase_seg2 - 1)
            .with_wakfil(self.config.wake_filter)
            .with_sof(false);
        self.bus.write_register(REG_CNF1, cnf1.into_bits()).await?;
        self.bus.write_register(REG_CNF2, cnf2.into_bits()).await?;
        self.bus.write_register(REG_CNF3, cnf3.into_bits()).await?;
        Ok(())
    }

    /// Enables the interrupt sources [`Mcp25625::receive`] relies on (RX0,
    /// RX1, error, message-error, wake) and clears any CANINTF flags left
    /// over from before reset. TX-complete interrupts are deliberately left
    /// disabled: [`Mcp25625::transmit`] doesn't wait on them.
    async fn write_interrupt_enables(&mut self) -> Result<(), Mcp25625Error<E>> {
        let inte = CanIntE::new()
            .with_rx0(true)
            .with_rx1(true)
            .with_err(true)
            .with_merr(true)
            .with_wake(true);
        self.bus
            .write_register(REG_CANINTE, inte.into_bits())
            .await?;
        // Clear any stale flags left over from before reset.
        self.bus.write_register(REG_CANINTF, 0).await?;
        Ok(())
    }

    /// Applies `self.config.filter` to both RX buffers' RXM bits and both
    /// masks (RXM0, RXM1); for [`RxFilter::Match`] and [`RxFilter::Masked`],
    /// also fans the given IDs out across the 6 filter slots (RXF0..RXF5),
    /// repeating the last one into any slots beyond the number of IDs given.
    async fn write_filters(&mut self) -> Result<(), Mcp25625Error<E>> {
        let (rxm, mask_bytes): (u8, [u8; 4]) = match &self.config.filter {
            RxFilter::AcceptAll => (RXM_ANY, [0; 4]),
            RxFilter::Match(_) => (RXM_FILTERED, ID_MASK_ALL),
            RxFilter::Masked { mask, .. } => (RXM_FILTERED, encode_id(*mask)),
        };

        let rxb0 = Rxb0Ctrl::new()
            .with_rxm(rxm)
            .with_bukt(self.config.rollover_on_overflow);
        let rxb1 = Rxb1Ctrl::new().with_rxm(rxm);
        self.bus
            .write_register(RXB_CTRL[0], rxb0.into_bits())
            .await?;
        self.bus
            .write_register(RXB_CTRL[1], rxb1.into_bits())
            .await?;

        self.write_id_bytes(REG_RXM_SIDH[0], mask_bytes).await?;
        self.write_id_bytes(REG_RXM_SIDH[1], mask_bytes).await?;

        let ids = match &self.config.filter {
            RxFilter::AcceptAll => None,
            RxFilter::Match(ids) => Some(ids),
            RxFilter::Masked { ids, .. } => Some(ids),
        };
        if let Some(ids) = ids {
            if let Some(&last) = ids.last() {
                // Snapshot the per-slot IDs before writing: `ids` borrows
                // `self.config`, and each write below needs `&mut self`.
                let mut slots = [last; 6];
                for (slot, value) in slots.iter_mut().enumerate() {
                    *value = ids.get(slot).copied().unwrap_or(last);
                }
                for (addr, id) in REG_RXF_SIDH.iter().zip(slots) {
                    self.write_id_bytes(*addr, encode_id(id)).await?;
                }
            }
        }
        Ok(())
    }

    /// Writes 4 consecutive ID bytes (SIDH, SIDL, EID8, EID0, in that
    /// register order) starting at `sidh_addr`. Used for both filters
    /// (RXFn) and masks (RXMn), whose 4 ID registers are laid out
    /// identically to a TX/RX buffer's own ID bytes.
    async fn write_id_bytes(
        &mut self,
        sidh_addr: u8,
        bytes: [u8; 4],
    ) -> Result<(), Mcp25625Error<E>> {
        for (i, b) in bytes.iter().enumerate() {
            self.bus.write_register(sidh_addr + i as u8, *b).await?;
        }
        Ok(())
    }

    /// Requests a mode change and blocks (asynchronously) until the device
    /// confirms it, or returns [`Mcp25625Error::ModeTimeout`] if it never does.
    pub async fn set_mode(&mut self, mode: OpMode) -> Result<(), Mcp25625Error<E>> {
        let ctrl = CanCtrl::new()
            .with_reqop(mode)
            .with_osm(self.config.one_shot);
        self.bus
            .write_register(REG_CANCTRL, ctrl.into_bits())
            .await?;

        let mut loops = MAX_LOOPS;
        loop {
            let stat = CanStat::from_bits(self.bus.read_register(REG_CANSTAT).await?);
            if stat.opmod() == mode {
                self.config.mode = mode;
                return Ok(());
            }
            loops -= 1;
            if loops == 0 {
                warn!("MCP25625: mode change to {} timed out", mode);
                return Err(Mcp25625Error::ModeTimeout);
            }
            self.delay.delay_us(10).await;
        }
    }

    /// Queues `frame` for transmission on the first free TX buffer.
    ///
    /// Returns once the frame has been handed to the controller (RTS sent);
    /// it does not wait for the frame to actually win arbitration or be
    /// acknowledged on the bus. Returns [`Mcp25625Error::TxBusy`] if all 3 TX
    /// buffers remain occupied for the polling budget.
    ///
    /// Crate-internal building block for [`Mcp25625Runner`](crate::Mcp25625Runner)'s
    /// forwarding loop; see [`Mcp25625::transmit`] for the public, `raw-can`
    /// feature-gated equivalent.
    pub(crate) async fn transmit_impl(&mut self, frame: &CanFrame) -> Result<(), Mcp25625Error<E>> {
        let mut loops = MAX_LOOPS;
        let idx = loop {
            // The TXREQ bits are at 2, 4, 6 for TXB0, TXB1, TXB2 respectively
            // (see `Bus::read_status`'s doc for the full byte layout); a
            // clear bit means that buffer isn't currently pending a send.
            let status = self.bus.read_status().await?;
            if let Some(idx) = (0..3u8).find(|i| status & (0b0000_0100 << (2 * i)) == 0) {
                break idx;
            }
            loops -= 1;
            if loops == 0 {
                warn!("MCP25625: all TX buffers busy, dropping frame");
                self.log_tx_diagnostics().await;
                return Err(Mcp25625Error::TxBusy);
            }
            self.delay.delay_us(10).await;
        };

        let id_bytes = encode_id(frame.id());
        let dlc_byte = ((frame.is_remote_frame() as u8) << 6) | (frame.dlc() as u8 & 0x0F);
        let header = [id_bytes[0], id_bytes[1], id_bytes[2], id_bytes[3], dlc_byte];
        self.bus.load_tx_buffer(idx, header, frame.data()).await?;
        self.bus.request_to_send(idx).await?;
        trace!("MCP25625: queued frame on TXB{}, dlc={}", idx, frame.dlc());
        Ok(())
    }

    /// Reads back all 3 TXBnCTRL registers plus TEC/REC/EFLG and logs them,
    /// to tell apart *why* [`Mcp25625::transmit_impl`] found every TX buffer
    /// still busy: `mloa` (arbitration lost) is expected on a live bus with
    /// more than one active transmitter and should resolve on its own,
    /// whereas `txerr` (no ACK, or a CRC/form/stuff error) usually points to
    /// a wiring, termination, or bit-timing problem instead. Best-effort —
    /// SPI errors while reading these are swallowed, since this only runs
    /// after a real error is already about to be reported. A no-op without
    /// `defmt-messages`, since there'd be nowhere for the result to go.
    #[cfg(feature = "defmt-messages")]
    async fn log_tx_diagnostics(&mut self) {
        for (i, addr) in TXB_CTRL.iter().enumerate() {
            if let Ok(bits) = self.bus.read_register(*addr).await {
                warn!("MCP25625: TXB{}CTRL = {}", i, TxbCtrl::from_bits(bits));
            }
        }
        if let Ok(eflg) = self.bus.read_register(REG_EFLG).await {
            warn!("MCP25625: EFLG = {}", Eflg::from_bits(eflg));
        }
        if let Ok((tec, rec)) = self.error_counters().await {
            warn!("MCP25625: TEC = {}, REC = {}", tec, rec);
        }
    }

    #[cfg(not(feature = "defmt-messages"))]
    async fn log_tx_diagnostics(&mut self) {}

    /// Waits for the INT line to be asserted (active low).
    ///
    /// Checks the current level first rather than unconditionally awaiting a
    /// falling edge: the MCP25625's INT pin is level-driven (asserted for as
    /// long as any enabled interrupt flag is latched), so if it is already
    /// low when this is called — e.g. another flag is still pending right
    /// after the previous iteration cleared a different one — an
    /// edge-triggered `Wait` implementation could otherwise block forever
    /// waiting for a falling edge that will never come.
    async fn wait_for_irq(&mut self) {
        if matches!(self.irq.is_low(), Ok(true)) {
            return;
        }
        let _ = self.irq.wait_for_low().await;
    }

    /// Waits for the interrupt line to signal a pending event, then returns
    /// the first received frame. Bus errors observed while waiting are
    /// reported (and their EFLG latch cleared) as
    /// [`Mcp25625Error::Bus`] instead of being retried silently.
    ///
    /// Crate-internal building block for [`Mcp25625Runner`](crate::Mcp25625Runner)'s
    /// forwarding loop; see [`Mcp25625::receive`] for the public, `raw-can`
    /// feature-gated equivalent.
    pub(crate) async fn receive_impl(&mut self) -> Result<CanFrame, Mcp25625Error<E>> {
        loop {
            self.wait_for_irq().await;
            let intf = CanIntF::from_bits(self.bus.read_register(REG_CANINTF).await?);

            if intf.merr() || intf.err() {
                let eflg = Eflg::from_bits(self.bus.read_register(REG_EFLG).await?);
                self.bus.write_register(REG_EFLG, 0).await?;
                let clear = CanIntF::new().with_err(true).with_merr(true);
                self.bus
                    .modify_register(REG_CANINTF, clear.into_bits(), 0)
                    .await?;
                warn!("MCP25625: bus error, EFLG={}", eflg);
                return Err(Mcp25625Error::Bus(eflg));
            }

            if intf.rx0() {
                let frame = self.read_rx_frame(0).await?;
                let clear = CanIntF::new().with_rx0(true);
                self.bus
                    .modify_register(REG_CANINTF, clear.into_bits(), 0)
                    .await?;
                trace!("MCP25625: received frame on RXB0, dlc={}", frame.dlc());
                return Ok(frame);
            }

            if intf.rx1() {
                let frame = self.read_rx_frame(1).await?;
                let clear = CanIntF::new().with_rx1(true);
                self.bus
                    .modify_register(REG_CANINTF, clear.into_bits(), 0)
                    .await?;
                trace!("MCP25625: received frame on RXB1, dlc={}", frame.dlc());
                return Ok(frame);
            }

            // Only TX-complete/wake flags are pending (if their interrupts are
            // enabled): clear everything else so the IRQ line deasserts, then
            // keep waiting for an actual receive event.
            if intf.into_bits() != 0 {
                self.bus.write_register(REG_CANINTF, 0).await?;
            }
        }
    }

    /// Reads the transmit and receive error counters (TEC, REC). Both
    /// saturate at 255; per the CAN spec, TEC >= 256 (latched as 255 here)
    /// means the device has gone bus-off, also reflected in
    /// [`Eflg::txbo`] when returned via [`Mcp25625Error::Bus`].
    pub async fn error_counters(&mut self) -> Result<(u8, u8), Mcp25625Error<E>> {
        let tec = self.bus.read_register(REG_TEC).await?;
        let rec = self.bus.read_register(REG_REC).await?;
        Ok((tec, rec))
    }

    /// Reads RX buffer `idx` (0 or 1) in full and decodes it into a
    /// [`CanFrame`]. Does not clear the corresponding CANINTF flag — callers
    /// (currently only `receive_impl`) are responsible for that, since
    /// they know which flag they're acting on.
    async fn read_rx_frame(&mut self, idx: u8) -> Result<CanFrame, Mcp25625Error<E>> {
        let mut header = [0u8; 5];
        let mut data = [0u8; 8];
        self.bus.read_rx_buffer(idx, &mut header, &mut data).await?;
        let id = decode_id([header[0], header[1], header[2], header[3]]);
        let dlc_byte = header[4];
        let rtr = dlc_byte & 0x40 != 0;
        let dlc = (dlc_byte & 0x0F).min(8);
        Ok(CanFrame::from_raw(id, rtr, dlc, data))
    }
}

/// Raw single-frame transmit/receive, gated behind the `raw-can` feature.
///
/// These bypass every peer/addressing/fragmentation concept the rest of this
/// crate provides: prefer [`crate::new_p2p`] (with [`crate::BusArbiter`]) or
/// [`crate::IsoTpChannel`] for actual communication, both of which are built
/// on the same underlying frames via [`crate::split`]. Enable this feature
/// only for bring-up/debugging or to build a protocol of your own.
#[cfg(feature = "raw-can")]
#[cfg_attr(docsrs, doc(cfg(feature = "raw-can")))]
impl<SPI, RST, IRQ, D, E> Mcp25625<SPI, RST, IRQ, D>
where
    SPI: SpiDevice<Error = E>,
    RST: OutputPin<Error = Infallible>,
    IRQ: InputPin<Error = Infallible> + Wait<Error = Infallible>,
    D: DelayNs,
{
    /// Queues `frame` for transmission on the first free TX buffer.
    ///
    /// Returns once the frame has been handed to the controller (RTS sent);
    /// it does not wait for the frame to actually win arbitration or be
    /// acknowledged on the bus. Returns [`Mcp25625Error::TxBusy`] if all 3 TX
    /// buffers remain occupied for the polling budget.
    pub async fn transmit(&mut self, frame: &CanFrame) -> Result<(), Mcp25625Error<E>> {
        self.transmit_impl(frame).await
    }

    /// Waits for the interrupt line to signal a pending event, then returns
    /// the first received frame. Bus errors observed while waiting are
    /// reported (and their EFLG latch cleared) as
    /// [`Mcp25625Error::Bus`] instead of being retried silently.
    pub async fn receive(&mut self) -> Result<CanFrame, Mcp25625Error<E>> {
        self.receive_impl().await
    }
}

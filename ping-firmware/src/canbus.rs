//! CAN bus bring-up: brings up the MCP25625 over SPI0 exactly like the main
//! firmware's `canbus` module and `chat-firmware`'s `canbus_chat` module
//! (same clock and bit timing). No p2p/runner stack here — `main`'s ping
//! loop drives the raw driver directly.

use defmt::{info, warn};
use embassy_rp::{
    clocks::{Gpout, GpoutSrc},
    gpio::{Input, Level, Output, Pull},
    peripherals::SPI0,
    spi::{self, Async, Spi},
};
use embassy_time::{Delay, Timer};
use embedded_hal_bus::spi::{ExclusiveDevice, NoDelay};
use mcp25625::{BitTiming, Mcp25625, Mcp25625Config, OperatingMode, RxFilter};

use crate::resources::{CanDev, IrqHandlers};

type CanSpiBus = Spi<'static, SPI0, Async>;
type CanSpiDevice = ExclusiveDevice<CanSpiBus, Output<'static>, NoDelay>;
/// Raw MCP25625 driver type, exposing `mcp25625`'s single-frame
/// `transmit`/`receive` (the `raw-can` feature) directly to `main`.
pub type CanDriver = Mcp25625<CanSpiDevice, Output<'static>, Input<'static>, Delay>;

/// Configures the CAN reference clock and SPI peripheral, constructs the
/// driver with `filter`/`mode` applied, and retries [`Mcp25625::init`]
/// forever on failure — there is nothing else this firmware can usefully do
/// without a working bus. `mode` is exposed (rather than hardcoding
/// `OperatingMode::Normal`) so `main` can select [`OperatingMode::Loopback`]
/// under the `loopback` feature: a self-test that routes TX straight back to
/// RX internally, without touching CAN_H/CAN_L, to check the SPI/interrupt/
/// buffer-read path in isolation from the physical bus.
pub async fn bring_up(can_dev: CanDev, filter: RxFilter, mode: OperatingMode) -> CanDriver {
    // Generate the MCP25625's OSC1 reference clock: 48 MHz (PllUsb) / 3 = 16 MHz.
    let refck = Gpout::new(can_dev.refclk);
    refck.set_src(GpoutSrc::PllUsb);
    refck.set_div(3, 0);
    refck.enable();
    // This only confirms the RP2350's own clock-tree math — not that the
    // signal actually reaches the MCP25625's OSC1 pin. If transmissions
    // never complete (TXREQ stuck, but no txerr/mloa/EFLG/TEC/REC ever
    // move) even in Loopback mode, which never touches CAN_H/CAN_L, that
    // points at OSC1 not toggling on the chip side: check this trace with a
    // scope, and that OSC2 is left floating rather than grounded (required
    // when driving OSC1 externally instead of using a crystal).
    info!(
        "CAN: reference clock enabled, configured frequency = {} Hz",
        refck.get_freq()
    );

    let mut spi_config = spi::Config::default();
    spi_config.frequency = 8_000_000; // well within the MCP25625's SPI clock rating
    spi_config.phase = spi::Phase::CaptureOnFirstTransition; // SPI mode 0,0
    spi_config.polarity = spi::Polarity::IdleLow;

    let spi_bus = Spi::new(
        can_dev.spi,
        can_dev.clk,
        can_dev.tx,
        can_dev.rx,
        can_dev.dma_tx,
        can_dev.dma_rx,
        IrqHandlers,
        spi_config,
    );
    let cs = Output::new(can_dev.cs, Level::High);
    let spi_dev =
        ExclusiveDevice::new_no_delay(spi_bus, cs).expect("infallible CS pin, cannot fail");

    let rst = Output::new(can_dev.rst, Level::High);
    let irq = Input::new(can_dev.irq, Pull::Up);

    let bit_timing =
        BitTiming::from_bit_rate(16_000_000, 500_000).expect("500 kbps is achievable at 16 MHz");
    let config = Mcp25625Config::new(bit_timing).with_mode(mode);
    let mut driver: CanDriver = Mcp25625::new(spi_dev, rst, irq, Delay, config);
    driver.set_filter(filter);

    loop {
        match driver.init().await {
            Ok(()) => break,
            Err(e) => {
                warn!("CAN: MCP25625 init failed: {}; retrying in 1s", e);
                Timer::after_secs(1).await;
            }
        }
    }
    driver
}

//! CAN bus bring-up: brings up the MCP25625 over SPI0 exactly like the main
//! firmware's `canbus` module (same clock and bit timing), deriving this
//! node's peer address from the RP2350's own factory-programmed OTP chip ID
//! instead of a flashed device identifier — this crate has no configuration
//! step of its own, so there's nothing else stable to derive it from. Hands
//! the resulting peer-to-peer sender/receiver pair back to the caller
//! instead of driving its own demo loop on top of them.

use crc16::{State, XMODEM};
use defmt::{debug, error, info, warn};
use embassy_executor::Spawner;
use embassy_rp::{
    clocks::{Gpout, GpoutSrc},
    gpio::{Input, Level, Output, Pull},
    otp,
    peripherals::SPI0,
    spi::{self, Async, Spi},
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel, signal::Signal};
use embassy_time::{Delay, Timer};
use embedded_hal_bus::spi::{ExclusiveDevice, NoDelay};
use mcp25625::{
    BitTiming, CanFrame, MAX_PEERS, Mcp25625, Mcp25625Config, Mcp25625Runner, OperatingMode,
    P2pReceiver, P2pSender, PeerAddr, RxFilter, hardware_filter, new_p2p, split,
};
use static_cell::StaticCell;

use crate::resources::{CanDev, IrqHandlers};

/// Depth of the driver-level TX/RX queues between the runner task and the
/// p2p sender/receiver handed back to `chat_task`.
const CAN_QUEUE_LEN: usize = 8;

type CanSpiBus = Spi<'static, SPI0, Async>;
type CanSpiDevice = ExclusiveDevice<CanSpiBus, Output<'static>, NoDelay>;
type CanDriver = Mcp25625<CanSpiDevice, Output<'static>, Input<'static>, Delay>;
type CanChannel = Channel<NoopRawMutex, CanFrame, CAN_QUEUE_LEN>;
type CanRunner = Mcp25625Runner<
    'static,
    CanSpiDevice,
    Output<'static>,
    Input<'static>,
    Delay,
    NoopRawMutex,
    CAN_QUEUE_LEN,
    CAN_QUEUE_LEN,
>;
/// Peer-to-peer sender for chat messages — cheaply `Clone`, so `chat_task`
/// can hand copies to as many concurrent senders as it needs (currently just
/// itself).
pub type CanSender = P2pSender<'static, NoopRawMutex, CAN_QUEUE_LEN, CAN_QUEUE_LEN>;
/// Peer-to-peer receiver for chat messages — meant to be polled by a single
/// task, same as everywhere else this driver is used.
pub type CanReceiver = P2pReceiver<'static, NoopRawMutex, CAN_QUEUE_LEN, CAN_QUEUE_LEN>;

/// Reads the RP2350's 64-bit factory-programmed OTP chip ID — stable across
/// resets and unique per board, so it's a reasonable stand-in for this
/// crate's not having its own flashed device identifier to derive a peer
/// address from (contrast `firmware/src/canbus.rs`'s `derive_peer_addr`,
/// which uses `DEVICE_ID` loaded from flash by the main firmware instead).
/// Falls back to an arbitrary fixed value (logging a warning) if the OTP
/// read itself fails, so a node without a well-formed chip ID still comes
/// up rather than getting stuck.
fn read_chip_id() -> u64 {
    match otp::get_chipid() {
        Ok(id) => id,
        Err(e) => {
            warn!(
                "CAN: failed to read OTP chip ID: {:?}; peer address may collide",
                e
            );
            0
        }
    }
}

/// Derives this node's p2p peer address (`0..MAX_PEERS`) from the chip ID's
/// CRC16/XMODEM checksum, reduced mod [`MAX_PEERS`] — same approach as the
/// main firmware's `canbus` module, just fed different input bytes (see
/// [`read_chip_id`]).
fn derive_peer_addr(chip_id: u64) -> PeerAddr {
    let crc = State::<XMODEM>::calculate(&chip_id.to_be_bytes());
    let addr = (crc as usize % MAX_PEERS) as u8;
    PeerAddr::new(addr).expect("reduced mod MAX_PEERS, always in range")
}

/// Configures the CAN reference clock and SPI peripheral, brings up the
/// MCP25625 (retrying forever on failure, since there is nothing else for
/// this firmware to usefully do without a working bus), and spawns its
/// runner task. Returns a p2p sender/receiver pair for `chat_task` to
/// broadcast/receive chat messages on directly.
pub async fn init(spawner: Spawner, can_dev: CanDev) -> (CanSender, CanReceiver) {
    let chip_id = read_chip_id();
    let local = derive_peer_addr(chip_id);

    // Generate the MCP25625's OSC1 reference clock: 48 MHz (PllUsb) / 3 = 16 MHz.
    let refck = Gpout::new(can_dev.refclk);
    refck.set_src(GpoutSrc::PllUsb);
    refck.set_div(3, 0);
    refck.enable();

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
    let config = Mcp25625Config::new(bit_timing).with_mode(OperatingMode::Normal);
    let mut driver: CanDriver = Mcp25625::new(spi_dev, rst, irq, Delay, config);
    driver.set_filter(hardware_filter(local));

    loop {
        match driver.init().await {
            Ok(()) => break,
            Err(e) => {
                warn!("CAN: MCP25625 init failed: {}; retrying in 1s", e);
                Timer::after_secs(1).await;
            }
        }
    }
    info!("CAN: MCP25625 initialized as peer {}", local.as_u8());

    static TX_CHANNEL: StaticCell<CanChannel> = StaticCell::new();
    static RX_CHANNEL: StaticCell<CanChannel> = StaticCell::new();
    static FILTER_SIGNAL: StaticCell<Signal<NoopRawMutex, RxFilter>> = StaticCell::new();
    let tx_channel = TX_CHANNEL.init(Channel::new());
    let rx_channel = RX_CHANNEL.init(Channel::new());
    let filter_signal = FILTER_SIGNAL.init(Signal::new());

    let (handle, runner) = split(driver, tx_channel, rx_channel, filter_signal);
    match can_runner_task(runner) {
        Ok(token) => {
            spawner.spawn(token);
            debug!("CAN runner task spawned");
        }
        Err(_) => error!("Failed to spawn CAN runner task"),
    }

    new_p2p(handle, local)
}

#[embassy_executor::task]
async fn can_runner_task(runner: CanRunner) -> ! {
    runner.run().await
}

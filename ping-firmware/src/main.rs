#![no_std]
#![no_main]
//! Standalone CAN ping/debug firmware: broadcasts an 8-byte frame of true
//! hardware-random data once a second, and logs every frame received from
//! other nodes as it arrives. Talks to the MCP25625 directly via
//! `mcp25625`'s raw single-frame `Mcp25625::transmit`/`receive` (the
//! `raw-can` feature), instead of the p2p/ISO-TP protocols built on top of
//! `split`/`Mcp25625Runner` that `chat-firmware` uses — for confirming
//! basic frame transfer works on the physical bus before trusting that
//! higher-level stack.
//!
//! Deliberately minimal: no chat/USB-terminal interface, no p2p addressing
//! — just the raw driver and a defmt log sink to watch it with.

use defmt::{info, warn};
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_rp::{
    bind_interrupts,
    peripherals::TRNG,
    trng::{Config as TrngConfig, InterruptHandler as TrngInterruptHandler, Trng},
};
use embassy_time::{Duration, Ticker, Timer};
use mcp25625::{CanFrame, OperatingMode, RxFilter, StandardId};
use panic_probe as _;

mod canbus;
mod resources;
mod usb_log;

use resources::{AssignedResources, CanDev, UsbConfDev};

/// How often a ping frame is broadcast.
const PING_INTERVAL: Duration = Duration::from_secs(1);
/// Arbitrary standard ID for these debug frames — distinct from anything
/// the p2p protocol's one-hot peer addressing would use, so this doesn't
/// get mistaken for (or interfere with) chat traffic sharing the same bus.
const PING_ID: u16 = 0x555;

bind_interrupts!(struct TrngIrqs {
    TRNG_IRQ => TrngInterruptHandler<TRNG>;
});

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let resources = split_resources!(p);

    // Give an attached debug probe time to parse the defmt RTT header before
    // any flash access, same reasoning as `chat-firmware`'s `main`.
    Timer::after_millis(10).await;

    usb_log::build(spawner, resources.usbconfig);
    // Let USB enumerate before relying on defmt logging over it.
    Timer::after_millis(1000).await;

    info!(
        "Kiwi CAN Ping Firmware v{} starting up...",
        env!("CARGO_PKG_VERSION")
    );

    // Under the `loopback` feature, run in Loopback mode instead of Normal:
    // TX is routed straight back to RX internally, without touching
    // CAN_H/CAN_L, so a single board can confirm its own SPI/interrupt/
    // buffer-read path works before blaming the physical bus, wiring, or
    // the other node.
    #[cfg(feature = "loopback")]
    let mode = OperatingMode::Loopback;
    #[cfg(not(feature = "loopback"))]
    let mode = OperatingMode::Normal;

    // Accept every frame regardless of destination (not just ones addressed
    // to some p2p peer address) so any other node's raw traffic shows up.
    let mut driver = canbus::bring_up(resources.can, RxFilter::AcceptAll, mode).await;
    info!("CAN: MCP25625 initialized in {} mode", mode);

    let mut config = TrngConfig::default();
    config.sample_count = 8192;
    let mut trng = Trng::new(p.TRNG, TrngIrqs, config);
    let id = StandardId::new(PING_ID).expect("fits 11 bits");
    let mut ticker = Ticker::every(PING_INTERVAL);

    loop {
        match select(ticker.next(), driver.receive()).await {
            Either::First(()) => {
                let mut data = [0u8; 8];
                trng.fill_bytes(&mut data).await;
                let frame = CanFrame::new(id, &data).expect("8 bytes fits a classic CAN frame");
                match driver.transmit(&frame).await {
                    // `Ok` here only means RTS was accepted, not that the
                    // frame was ever ACKed on the bus — TEC/REC (which climb
                    // on repeated ACK/bit/CRC errors) are logged alongside it
                    // so a silently-failing transmit doesn't look identical
                    // to a genuinely successful one.
                    Ok(()) => match driver.error_counters().await {
                        Ok((tec, rec)) => {
                            info!("PING: sent {:02x} (TEC={}, REC={})", data, tec, rec)
                        }
                        Err(_) => info!("PING: sent {:02x}", data),
                    },
                    Err(e) => warn!("PING: failed to send: {}", e),
                }
            }
            Either::Second(Ok(frame)) => {
                info!("PING: received {}", frame);
            }
            Either::Second(Err(e)) => {
                warn!("PING: bus error while receiving: {}", e);
            }
        }
    }
}

//! A single-owner, point-to-point [`mcp25625::IsoTpChannel`] conversation
//! carrying a payload bigger than one 8-byte CAN frame, running against the
//! mock hardware in `examples/common` instead of a real bus.
//!
//! `sender` (`StandardId` `0x100`) segments a message across a First Frame
//! and several Consecutive Frames; `receiver` (`StandardId` `0x101`)
//! reassembles it, waiting on `sender`'s Flow Control pacing, then replies
//! with a short acknowledgement. Unlike [`mcp25625::new_p2p`], ISO-TP has no
//! concept of peer addresses — the two sides just agree on which CAN IDs
//! they transmit/expect on. Run with:
//!
//! ```text
//! cargo run -p mcp25625 --no-default-features --example isotp_transfer
//! ```
#[path = "common/mod.rs"]
mod common;

use common::{bus_pump, virtual_pair, NodeHardware};
use embassy_executor::{Executor, Spawner};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel, signal::Signal};
use mcp25625::{
    split, BitTiming, CanFrame, IsoTpChannel, IsoTpConfig, Mcp25625, Mcp25625Config, OperatingMode,
    RxFilter, StandardId,
};

const QUEUE_LEN: usize = 8;
/// Comfortably larger than one classic CAN frame's 8 bytes, so this
/// actually exercises First Frame + Consecutive Frame segmentation.
const MAX_LEN: usize = 128;

type CanChannel = Channel<NoopRawMutex, CanFrame, QUEUE_LEN>;
type IsoTp = IsoTpChannel<'static, NoopRawMutex, QUEUE_LEN, QUEUE_LEN, common::MockDelay, MAX_LEN>;

const MESSAGE: &[u8] =
    b"This ISO-TP payload is well over 8 bytes, so it has to be split across multiple frames.";

fn main() {
    let executor: &'static mut Executor = Box::leak(Box::new(Executor::new()));
    executor.run(|spawner| {
        let (hw_sender, hw_receiver) = virtual_pair();
        // sender transmits on 0x100 and expects replies on 0x101; receiver
        // is configured the mirror image of that.
        let sender_config = IsoTpConfig {
            tx_id: StandardId::new(0x100).unwrap().into(),
            rx_id: StandardId::new(0x101).unwrap().into(),
            ..Default::default()
        };
        let receiver_config = IsoTpConfig {
            tx_id: StandardId::new(0x101).unwrap().into(),
            rx_id: StandardId::new(0x100).unwrap().into(),
            ..Default::default()
        };
        spawner.spawn(sender_task(hw_sender, sender_config, spawner).unwrap());
        spawner.spawn(receiver_task(hw_receiver, receiver_config, spawner).unwrap());
    });
}

async fn build_channel(hw: NodeHardware, config: IsoTpConfig, spawner: Spawner) -> IsoTp {
    spawner.spawn(bus_pump(hw.chip, hw.inbox).unwrap());

    let bit_timing =
        BitTiming::from_bit_rate(16_000_000, 500_000).expect("500 kbps is achievable at 16 MHz");
    let mcp_config = Mcp25625Config::new(bit_timing).with_mode(OperatingMode::Normal);
    let mut driver = Mcp25625::new(hw.spi, hw.rst, hw.irq, hw.delay, mcp_config);
    driver.init().await.expect("mock chip init never fails");

    let tx_channel: &'static CanChannel = Box::leak(Box::new(Channel::new()));
    let rx_channel: &'static CanChannel = Box::leak(Box::new(Channel::new()));
    let filter_signal: &'static Signal<NoopRawMutex, RxFilter> = Box::leak(Box::new(Signal::new()));
    let (handle, runner) = split(driver, tx_channel, rx_channel, filter_signal);
    spawner.spawn(runner_task(runner).unwrap());

    IsoTpChannel::new(handle, embassy_time::Delay, config)
}

#[embassy_executor::task]
async fn sender_task(hw: NodeHardware, config: IsoTpConfig, spawner: Spawner) {
    let mut channel = build_channel(hw, config, spawner).await;

    println!("sender: sending {} bytes", MESSAGE.len());
    channel.send(MESSAGE).await.expect("fits MAX_ISO_TP_LEN");

    let ack = channel.receive().await.expect("receiver acknowledges");
    println!(
        "sender: got ack {:?}",
        core::str::from_utf8(&ack).unwrap_or("<binary>")
    );
    std::process::exit(0);
}

#[embassy_executor::task]
async fn receiver_task(hw: NodeHardware, config: IsoTpConfig, spawner: Spawner) {
    let mut channel = build_channel(hw, config, spawner).await;

    let message = channel.receive().await.expect("sender's message arrives");
    println!(
        "receiver: got {} bytes: {:?}",
        message.len(),
        core::str::from_utf8(&message).unwrap_or("<binary>")
    );
    assert_eq!(&message[..], MESSAGE, "round-tripped message must match");

    channel
        .send(b"ack")
        .await
        .expect("short reply fits a Single Frame");
}

#[embassy_executor::task(pool_size = 2)]
async fn runner_task(
    runner: mcp25625::Mcp25625Runner<
        'static,
        common::MockSpi,
        common::MockRst,
        common::MockIrq,
        common::MockDelay,
        NoopRawMutex,
        QUEUE_LEN,
        QUEUE_LEN,
    >,
) -> ! {
    runner.run().await
}

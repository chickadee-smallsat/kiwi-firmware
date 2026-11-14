//! Two peers exchanging datagrams over [`mcp25625::new_p2p`], running
//! against the mock hardware in `examples/common` instead of a real bus.
//!
//! `alice` (`PeerAddr::Peer0`) broadcasts a `ping` every second; `bob`
//! (`PeerAddr::Peer1`) replies with a unicast `pong` addressed straight back
//! to her. After a few round trips both sides exit. Run with:
//!
//! ```text
//! cargo run -p mcp25625 --no-default-features --example p2p_chat
//! ```
//!
//! (`--no-default-features` disables this crate's `defmt` feature, which
//! needs an embedded logger backend these std examples don't have.)
#[path = "common/mod.rs"]
mod common;

use common::{bus_pump, virtual_pair, NodeHardware};
use embassy_executor::{Executor, Spawner};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel, signal::Signal};
use embassy_time::{Duration, Ticker};
use mcp25625::{
    hardware_filter, new_p2p, split, BitTiming, CanFrame, Mcp25625, Mcp25625Config, OperatingMode,
    PeerAddr, RxFilter,
};

const QUEUE_LEN: usize = 8;
/// How many `ping`s alice sends before both nodes call it a demo.
const ROUNDS: u32 = 5;

type CanChannel = Channel<NoopRawMutex, CanFrame, QUEUE_LEN>;

#[derive(Clone, Copy)]
enum Role {
    Alice,
    Bob,
}

fn main() {
    let executor: &'static mut Executor = Box::leak(Box::new(Executor::new()));
    executor.run(|spawner| {
        let (hw_alice, hw_bob) = virtual_pair();
        spawner.spawn(node_task(hw_alice, PeerAddr::Peer0, Role::Alice, spawner).unwrap());
        spawner.spawn(node_task(hw_bob, PeerAddr::Peer1, Role::Bob, spawner).unwrap());
    });
}

#[embassy_executor::task(pool_size = 2)]
async fn node_task(hw: NodeHardware, local: PeerAddr, role: Role, spawner: Spawner) {
    spawner.spawn(bus_pump(hw.chip, hw.inbox).unwrap());

    let bit_timing =
        BitTiming::from_bit_rate(16_000_000, 500_000).expect("500 kbps is achievable at 16 MHz");
    let config = Mcp25625Config::new(bit_timing).with_mode(OperatingMode::Normal);
    let mut driver = Mcp25625::new(hw.spi, hw.rst, hw.irq, hw.delay, config);
    // Set before `init()`, same as `firmware/src/canbus.rs`: filters are only
    // written to the device during initialization.
    driver.set_filter(hardware_filter(local));
    driver.init().await.expect("mock chip init never fails");

    let tx_channel: &'static CanChannel = Box::leak(Box::new(Channel::new()));
    let rx_channel: &'static CanChannel = Box::leak(Box::new(Channel::new()));
    let filter_signal: &'static Signal<NoopRawMutex, RxFilter> = Box::leak(Box::new(Signal::new()));
    let (handle, runner) = split(driver, tx_channel, rx_channel, filter_signal);
    spawner.spawn(runner_task(runner).unwrap());

    let (sender, mut receiver) = new_p2p(handle, local);

    match role {
        Role::Alice => {
            let mut ticker = Ticker::every(Duration::from_secs(1));
            for round in 1..=ROUNDS {
                ticker.next().await;
                println!("alice: broadcasting ping #{round}");
                sender.broadcast(b"ping").await.expect("fits App channel");
                let (from, _channel, payload) = receiver.receive().await;
                println!(
                    "alice: {:?} from peer {}",
                    core::str::from_utf8(&payload).unwrap_or("<binary>"),
                    from.as_u8()
                );
            }
            println!("alice: done");
        }
        Role::Bob => {
            for _ in 1..=ROUNDS {
                let (from, _channel, payload) = receiver.receive().await;
                println!(
                    "bob: {:?} from peer {}, replying pong",
                    core::str::from_utf8(&payload).unwrap_or("<binary>"),
                    from.as_u8()
                );
                sender.send(from, b"pong").await.expect("fits App channel");
            }
            println!("bob: done");
            // Both sides have now completed their exchanges; exit the whole
            // process here since `Executor::run` never returns on its own.
            std::process::exit(0);
        }
    }
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

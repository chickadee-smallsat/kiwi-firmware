//! [`mcp25625::TimeSyncMaster`]/[`mcp25625::TimeSyncFollower`] running on top
//! of [`mcp25625::new_p2p`], against the mock hardware in `examples/common`
//! instead of a real bus.
//!
//! `master` (`PeerAddr::Peer0`) broadcasts a Sync every 500ms; `follower`
//! (`PeerAddr::Peer1`) tracks its offset from that, and additionally issues
//! a Delay Request every 700ms to refine it with a path-delay-compensated
//! measurement. To make the result legible, `follower`'s clock is
//! deliberately run 250ms ahead of `master`'s (real boards would of course
//! each have their own free-running clock; here both share one process's
//! wall clock, so the skew is added by hand) — once refined, `follower`'s
//! reported offset should read close to -250ms (subtract 250ms from its own
//! clock to get master time), with a mean path delay close to zero, since
//! there's no real bus transit time in this mock. Run with:
//!
//! ```text
//! cargo run -p mcp25625 --no-default-features --example timesync
//! ```
#[path = "common/mod.rs"]
mod common;

use common::{bus_pump, virtual_pair, NodeHardware};
use embassy_executor::{Executor, Spawner};
use embassy_sync::{
    blocking_mutex::{raw::NoopRawMutex, Mutex},
    channel::Channel,
    signal::Signal,
};
use embassy_time::{Duration, Timer};
use mcp25625::{
    new_p2p, run_periodic_delay_requests, split, BitTiming, CanFrame, Clock, ForwardedMessage,
    Mcp25625, Mcp25625Config, OperatingMode, P2pSender, PeerAddr, ProtocolRunner, RxFilter,
    SharedTimeSyncFollower, TimeSyncFollower, TimeSyncMaster,
};
use std::cell::RefCell;

const QUEUE_LEN: usize = 8;
const FORWARD_QUEUE_LEN: usize = 4;

type CanChannel = Channel<NoopRawMutex, CanFrame, QUEUE_LEN>;
type Sender = P2pSender<'static, NoopRawMutex, QUEUE_LEN, QUEUE_LEN>;
type ForwardChannel = Channel<NoopRawMutex, ForwardedMessage, FORWARD_QUEUE_LEN>;
// This example uses only the time-sync half of `ProtocolRunner`, so the bus
// arbitration lifetimes are never actually borrowed — any lifetime (here
// `'static`) works, since `peer`/`arbiter_role` are always `None` below.
type TsRunner = ProtocolRunner<
    'static,
    'static,
    'static,
    'static,
    'static,
    NoopRawMutex,
    QUEUE_LEN,
    QUEUE_LEN,
    FORWARD_QUEUE_LEN,
    SkewedClock,
>;

/// This node's own clock reading, in microseconds — real boards would each
/// read their own free-running timer here; every node in this example
/// shares one wall clock, with an artificial `skew_us` added to stand in
/// for that difference (see the module docs).
#[derive(Clone, Copy)]
struct SkewedClock {
    skew_us: i64,
}

impl Clock for SkewedClock {
    fn now(&mut self) -> u64 {
        (embassy_time::Instant::now().as_micros() as i64 + self.skew_us) as u64
    }
}

fn main() {
    let executor: &'static mut Executor = Box::leak(Box::new(Executor::new()));
    executor.run(|spawner| {
        let (hw_master, hw_follower) = virtual_pair();
        spawner.spawn(master_task(hw_master, spawner).unwrap());
        spawner.spawn(follower_task(hw_follower, spawner).unwrap());
    });
}

#[embassy_executor::task]
async fn master_task(hw: NodeHardware, spawner: Spawner) {
    spawner.spawn(bus_pump(hw.chip, hw.inbox).unwrap());

    let bit_timing =
        BitTiming::from_bit_rate(16_000_000, 500_000).expect("500 kbps is achievable at 16 MHz");
    let config = Mcp25625Config::new(bit_timing).with_mode(OperatingMode::Normal);
    let mut driver = Mcp25625::new(hw.spi, hw.rst, hw.irq, hw.delay, config);
    driver.init().await.expect("mock chip init never fails");

    let tx_channel: &'static CanChannel = Box::leak(Box::new(Channel::new()));
    let rx_channel: &'static CanChannel = Box::leak(Box::new(Channel::new()));
    let filter_signal: &'static Signal<NoopRawMutex, RxFilter> = Box::leak(Box::new(Signal::new()));
    let (handle, runner) = split(driver, tx_channel, rx_channel, filter_signal);
    spawner.spawn(runner_task(runner).unwrap());

    let (sender, receiver) = new_p2p(handle, PeerAddr::Peer0);

    // Answers followers' Delay Requests; note this is a *separate*
    // `TimeSyncMaster` instance from the one `broadcast_task` owns below —
    // `handle_message` needs no sequence-number state of its own, so the
    // two roles don't need to share one.
    let forward_channel: &'static ForwardChannel = Box::leak(Box::new(Channel::new()));
    let (ts_runner, _forward): (TsRunner, _) = ProtocolRunner::new(
        receiver,
        sender.clone(),
        None,
        Some(TimeSyncMaster::new()),
        None,
        None,
        forward_channel,
        SkewedClock { skew_us: 0 },
    );
    spawner.spawn(timesync_runner_task(ts_runner).unwrap());

    spawner.spawn(broadcast_task(sender).unwrap());
}

#[embassy_executor::task]
async fn broadcast_task(sender: Sender) -> ! {
    TimeSyncMaster::new()
        .run_broadcast(sender, embassy_time::Delay, 500, SkewedClock { skew_us: 0 })
        .await
}

#[embassy_executor::task]
async fn follower_task(hw: NodeHardware, spawner: Spawner) {
    spawner.spawn(bus_pump(hw.chip, hw.inbox).unwrap());

    let bit_timing =
        BitTiming::from_bit_rate(16_000_000, 500_000).expect("500 kbps is achievable at 16 MHz");
    let config = Mcp25625Config::new(bit_timing).with_mode(OperatingMode::Normal);
    let mut driver = Mcp25625::new(hw.spi, hw.rst, hw.irq, hw.delay, config);
    driver.init().await.expect("mock chip init never fails");

    let tx_channel: &'static CanChannel = Box::leak(Box::new(Channel::new()));
    let rx_channel: &'static CanChannel = Box::leak(Box::new(Channel::new()));
    let filter_signal: &'static Signal<NoopRawMutex, RxFilter> = Box::leak(Box::new(Signal::new()));
    let (handle, runner) = split(driver, tx_channel, rx_channel, filter_signal);
    spawner.spawn(runner_task(runner).unwrap());

    let (sender, receiver) = new_p2p(handle, PeerAddr::Peer1);

    let follower: &'static SharedTimeSyncFollower<NoopRawMutex> =
        Box::leak(Box::new(Mutex::new(RefCell::new(TimeSyncFollower::new()))));

    let forward_channel: &'static ForwardChannel = Box::leak(Box::new(Channel::new()));
    let (ts_runner, _forward): (TsRunner, _) = ProtocolRunner::new(
        receiver,
        sender.clone(),
        Some(follower),
        None,
        None,
        None,
        forward_channel,
        SkewedClock { skew_us: 250_000 },
    );
    spawner.spawn(timesync_runner_task(ts_runner).unwrap());

    spawner.spawn(delay_request_task(follower, sender).unwrap());

    // Poll until a path-delay-refined measurement has completed, then report
    // the result and end the demo.
    loop {
        Timer::after(Duration::from_millis(200)).await;
        let (synced, offset, mean_path_delay) = follower.lock(|cell| {
            let f = cell.borrow();
            (f.is_synced(), f.offset(), f.mean_path_delay())
        });
        if !synced {
            continue;
        }
        if let Some(mean_path_delay) = mean_path_delay {
            println!(
                "follower: synced — offset {}us, mean path delay {}us",
                offset.expect("offset is set once synced"),
                mean_path_delay
            );
            std::process::exit(0);
        }
        println!(
            "follower: synced (Sync only) — offset {}us, awaiting delay measurement",
            offset.expect("offset is set once synced")
        );
    }
}

#[embassy_executor::task]
async fn delay_request_task(
    follower: &'static SharedTimeSyncFollower<NoopRawMutex>,
    sender: Sender,
) -> ! {
    run_periodic_delay_requests(
        follower,
        sender,
        embassy_time::Delay,
        700,
        SkewedClock { skew_us: 250_000 },
    )
    .await
}

#[embassy_executor::task(pool_size = 2)]
async fn timesync_runner_task(runner: TsRunner) -> ! {
    runner.run().await
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

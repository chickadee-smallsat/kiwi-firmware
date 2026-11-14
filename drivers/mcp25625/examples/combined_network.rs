//! `mcp25625`'s three highest-level protocols — [`mcp25625::new_p2p`],
//! [`mcp25625::BusArbiter`]/[`mcp25625::BusArbitrationPeer`], and
//! [`mcp25625::TimeSyncMaster`]/[`mcp25625::TimeSyncFollower`] — all in use
//! at once across two nodes, both driven through a single
//! [`mcp25625::ProtocolRunner`] each, running against the mock hardware in
//! `examples/common` instead of a real bus.
//!
//! - **`master`** (pinned to [`mcp25625::ARBITER_ADDR`]) is elected the bus
//!   arbiter (there being no other candidate, its own candidacy wins as soon
//!   as no heartbeat has been heard — see [`mcp25625::run_arbiter_supervisor`]),
//!   registers `listener` when it asks, broadcasts [`mcp25625::TimeSync`]
//!   Syncs, answers Delay Requests, and replies to ordinary P2P `ping`s with
//!   `pong` — all four at once, from one [`mcp25625::ProtocolRunner`].
//! - **`listener`** starts with no address at all: it uses
//!   [`mcp25625::bootstrap`] (bus arbitration's one-shot registration step)
//!   to obtain a real [`mcp25625::PeerAddr`] and confirm its
//!   [`mcp25625::Role`], then runs ordinary P2P chat plus a
//!   [`mcp25625::TimeSyncFollower`] refined by Delay Requests, again from a
//!   single [`mcp25625::ProtocolRunner`].
//!
//! Both nodes' [`mcp25625::ProtocolRunner`] is the sole consumer of their
//! [`mcp25625::P2pReceiver`] (as any consumer of one must be — see the crate
//! docs); what makes running all three protocols at once possible is that
//! every role [`mcp25625::ProtocolRunner::new`] takes — time-sync follower,
//! time-sync master, arbitration peer, arbiter — is independently optional,
//! so `master` can give it *both* of the arbitration roles *and* a
//! time-sync master role together, instead of needing a separate runner per
//! protocol that would each demand sole ownership of the same receiver.
//! `listener` only ever needs bus arbitration *once*, via [`mcp25625::bootstrap`]
//! — a self-contained function that's the sole consumer of a *temporary*
//! receiver for just that duration, built with [`mcp25625::UNASSIGNED_ADDR`]
//! and then dropped — so by the time it builds its real, permanently
//! addressed pair and its own `ProtocolRunner`, it only needs the time-sync
//! follower role.
//!
//! Run with:
//!
//! ```text
//! cargo run -p mcp25625 --no-default-features --example combined_network
//! ```
#[path = "common/mod.rs"]
mod common;

use common::{bus_pump, virtual_pair, NodeHardware};
use core::cell::RefCell;
use embassy_executor::{Executor, Spawner};
use embassy_sync::{
    blocking_mutex::{raw::NoopRawMutex, Mutex},
    channel::Channel,
    signal::Signal,
};
use embassy_time::{Duration, Ticker};
use mcp25625::{
    bootstrap, hardware_filter, new_p2p, run_arbiter_supervisor, run_periodic_delay_requests,
    split, BitTiming, BusArbitrationPeer, CanFrame, Clock, ForwardedMessage, Mcp25625,
    Mcp25625Config, OperatingMode, P2pChannel, P2pSender, PeerAddr, ProtocolRunner, Role, RxFilter,
    SharedBusArbiterRole, SharedBusArbitrationPeer, SharedTimeSyncFollower, TimeSyncFollower,
    TimeSyncMaster, ARBITER_ADDR, UNASSIGNED_ADDR,
};

const QUEUE_LEN: usize = 8;
const FORWARD_QUEUE_LEN: usize = 4;
/// How many `ping`s `listener` sends before the demo wraps up.
const ROUNDS: u32 = 5;

// Short, demo-scale timings — a real deployment would use much longer
// heartbeat/election windows relative to its actual message latency.
const HEARTBEAT_TIMEOUT_MS: u32 = 300;
const ELECTION_WINDOW_MS: u32 = 200;
const HEARTBEAT_PERIOD_MS: u32 = 400;
const SYNC_PERIOD_MS: u32 = 300;
const DELAY_REQUEST_PERIOD_MS: u32 = 350;
const BOOTSTRAP_RETRY_MS: u32 = 250;
const BOOTSTRAP_MAX_ATTEMPTS: u32 = 20;

type CanChannel = Channel<NoopRawMutex, CanFrame, QUEUE_LEN>;
type Sender = P2pSender<'static, NoopRawMutex, QUEUE_LEN, QUEUE_LEN>;
type ForwardChannel = Channel<NoopRawMutex, ForwardedMessage, FORWARD_QUEUE_LEN>;
// One runner type serves both nodes: `master` gives it `master`+`peer`+
// `arbiter_role` (and no `follower`), `listener` gives it just `follower` —
// every role is independently optional, so a single `ProtocolRunner` type
// covers any combination either node needs.
type Runner = ProtocolRunner<
    'static,
    'static,
    'static,
    'static,
    'static,
    NoopRawMutex,
    QUEUE_LEN,
    QUEUE_LEN,
    FORWARD_QUEUE_LEN,
    DemoClock,
>;

/// This node's own clock reading, in microseconds. Every node in this
/// example shares one wall clock (it's all one process); `skew_us` stands in
/// for the difference a real board's own free-running clock would have —
/// see `examples/timesync.rs` for the same idea used on its own.
#[derive(Clone, Copy)]
struct DemoClock {
    skew_us: i64,
}

impl Clock for DemoClock {
    fn now(&mut self) -> u64 {
        (embassy_time::Instant::now().as_micros() as i64 + self.skew_us) as u64
    }
}

/// A fixed candidacy priority: `master` is the only ever candidate in this
/// demo, so any constant value always wins its own (single-candidate)
/// election — a real multi-arbiter-capable deployment would source this
/// from something less predictable (e.g. a hardware RNG).
fn fixed_priority() -> u32 {
    42
}

fn main() {
    let executor: &'static mut Executor = Box::leak(Box::new(Executor::new()));
    executor.run(|spawner| {
        let (hw_master, hw_listener) = virtual_pair();
        spawner.spawn(master_task(hw_master, spawner).unwrap());
        spawner.spawn(listener_task(hw_listener, spawner).unwrap());
    });
}

#[embassy_executor::task]
async fn master_task(hw: NodeHardware, spawner: Spawner) {
    spawner.spawn(bus_pump(hw.chip, hw.inbox).unwrap());

    let bit_timing =
        BitTiming::from_bit_rate(16_000_000, 500_000).expect("500 kbps is achievable at 16 MHz");
    let config = Mcp25625Config::new(bit_timing).with_mode(OperatingMode::Normal);
    let mut driver = Mcp25625::new(hw.spi, hw.rst, hw.irq, hw.delay, config);
    // `master` is pinned to a fixed, known address from the start (unlike
    // `listener`, whose address isn't decided until after `bootstrap`
    // returns), so it can benefit from hardware acceptance filtering too.
    driver.set_filter(hardware_filter(ARBITER_ADDR));
    driver.init().await.expect("mock chip init never fails");

    let tx_channel: &'static CanChannel = Box::leak(Box::new(Channel::new()));
    let rx_channel: &'static CanChannel = Box::leak(Box::new(Channel::new()));
    let filter_signal: &'static Signal<NoopRawMutex, RxFilter> = Box::leak(Box::new(Signal::new()));
    let (handle, runner) = split(driver, tx_channel, rx_channel, filter_signal);
    spawner.spawn(runner_task(runner).unwrap());

    let (sender, receiver) = new_p2p(handle, ARBITER_ADDR);

    let peer: &'static SharedBusArbitrationPeer<NoopRawMutex> = Box::leak(Box::new(Mutex::new(
        RefCell::new(BusArbitrationPeer::new()),
    )));
    // Starts `None`: `master` isn't pre-installed as arbiter, it *wins
    // election* to become one below — the only candidate on this bus, so it
    // always wins, but it still goes through the same process a real
    // multi-node election would.
    let arbiter_role: &'static SharedBusArbiterRole<NoopRawMutex> =
        Box::leak(Box::new(Mutex::new(RefCell::new(None))));

    let forward_channel: &'static ForwardChannel = Box::leak(Box::new(Channel::new()));
    let (protocol_runner, forward): (Runner, _) = ProtocolRunner::new(
        receiver,
        sender.clone(),
        None,
        Some(TimeSyncMaster::new()),
        Some(peer),
        Some(arbiter_role),
        forward_channel,
        DemoClock { skew_us: 0 },
    );
    spawner.spawn(protocol_runner_task(protocol_runner).unwrap());

    spawner
        .spawn(arbiter_supervisor_task(peer, arbiter_role, sender.clone(), ARBITER_ADDR).unwrap());

    // Broadcasting is send-only, so it doesn't need to be the receiver's
    // sole consumer the way answering Delay Requests would — this instance
    // is independent of the one the `ProtocolRunner` above holds for that
    // purpose.
    spawner.spawn(broadcast_task(sender.clone()).unwrap());

    println!("master: waiting to be elected arbiter and broadcasting time sync");
    loop {
        let (src, channel, payload) = forward.receive().await;
        if channel == P2pChannel::App && payload.as_slice() == b"ping" {
            println!("master: ping from peer {}, replying pong", src.as_u8());
            let _ = sender.send(src, b"pong").await;
        }
    }
}

#[embassy_executor::task]
async fn broadcast_task(sender: Sender) -> ! {
    TimeSyncMaster::new()
        .run_broadcast(
            sender,
            embassy_time::Delay,
            SYNC_PERIOD_MS,
            DemoClock { skew_us: 0 },
        )
        .await
}

#[embassy_executor::task]
async fn arbiter_supervisor_task(
    peer: &'static SharedBusArbitrationPeer<NoopRawMutex>,
    arbiter_role: &'static SharedBusArbiterRole<NoopRawMutex>,
    sender: Sender,
    local: PeerAddr,
) -> ! {
    run_arbiter_supervisor(
        peer,
        arbiter_role,
        sender,
        local,
        embassy_time::Delay,
        HEARTBEAT_TIMEOUT_MS,
        ELECTION_WINDOW_MS,
        HEARTBEAT_PERIOD_MS,
        DemoClock { skew_us: 0 },
        fixed_priority,
    )
    .await
}

#[embassy_executor::task]
async fn listener_task(hw: NodeHardware, spawner: Spawner) {
    spawner.spawn(bus_pump(hw.chip, hw.inbox).unwrap());

    let bit_timing =
        BitTiming::from_bit_rate(16_000_000, 500_000).expect("500 kbps is achievable at 16 MHz");
    let config = Mcp25625Config::new(bit_timing).with_mode(OperatingMode::Normal);
    let mut driver = Mcp25625::new(hw.spi, hw.rst, hw.irq, hw.delay, config);
    // Left as the default `RxFilter::AcceptAll` for `init()`: unlike
    // `master`, this node's address isn't known until after `bootstrap`
    // returns below. It switches to a real, address-specific filter live
    // afterwards, via `Mcp25625Handle::update_filter` — see below.
    driver.init().await.expect("mock chip init never fails");

    let tx_channel: &'static CanChannel = Box::leak(Box::new(Channel::new()));
    let rx_channel: &'static CanChannel = Box::leak(Box::new(Channel::new()));
    let filter_signal: &'static Signal<NoopRawMutex, RxFilter> = Box::leak(Box::new(Signal::new()));
    let (handle, runner) = split(driver, tx_channel, rx_channel, filter_signal);
    spawner.spawn(runner_task(runner).unwrap());

    let role: Role = *b"LISTEN";
    let mut salt = 0u16;
    let (real_addr, granted_role) = {
        // Temporary pair at `UNASSIGNED_ADDR`, sole consumer of its own
        // receiver only for the duration of `bootstrap` — see the module
        // docs. `handle` is cheaply `Clone`d so the real pair below can
        // reuse the same underlying driver queues afterwards.
        let (boot_sender, mut boot_receiver) = new_p2p(handle.clone(), UNASSIGNED_ADDR);
        println!("listener: bootstrapping...");
        bootstrap(
            &mut boot_receiver,
            &boot_sender,
            role,
            || {
                salt = salt.wrapping_add(1);
                salt
            },
            embassy_time::Delay,
            BOOTSTRAP_RETRY_MS,
            BOOTSTRAP_MAX_ATTEMPTS,
        )
        .await
        .expect("master is reachable and LISTEN isn't already registered")
    };
    println!(
        "listener: registered as peer {} (role {:?})",
        real_addr.as_u8(),
        core::str::from_utf8(&granted_role).unwrap_or("<binary>")
    );
    // Now that a real address is known, narrow the hardware filter to it —
    // live, without a full re-`init()` (see `Mcp25625::update_filter`).
    handle.update_filter(hardware_filter(real_addr));

    let (sender, receiver) = new_p2p(handle, real_addr);

    let follower: &'static SharedTimeSyncFollower<NoopRawMutex> =
        Box::leak(Box::new(Mutex::new(RefCell::new(TimeSyncFollower::new()))));

    let forward_channel: &'static ForwardChannel = Box::leak(Box::new(Channel::new()));
    let (protocol_runner, forward): (Runner, _) = ProtocolRunner::new(
        receiver,
        sender.clone(),
        Some(follower),
        None,
        None,
        None,
        forward_channel,
        DemoClock { skew_us: 150_000 },
    );
    spawner.spawn(protocol_runner_task(protocol_runner).unwrap());

    // Unlike the old, separate-runner design this replaces, `master` running
    // bus arbitration doesn't stop it from also answering Delay Requests
    // (see the module docs) — so, unlike a version of this example that
    // predates `ProtocolRunner`, this actually completes.
    spawner.spawn(
        delay_request_task(follower, sender.clone(), DemoClock { skew_us: 150_000 }).unwrap(),
    );

    let mut ticker = Ticker::every(Duration::from_millis(500));
    for round in 1..=ROUNDS {
        ticker.next().await;
        println!("listener: broadcasting ping #{round}");
        let _ = sender.broadcast(b"ping").await;
        let (src, channel, payload) = forward.receive().await;
        if channel == P2pChannel::App {
            println!(
                "listener: {:?} from peer {}",
                core::str::from_utf8(&payload).unwrap_or("<binary>"),
                src.as_u8()
            );
        }
        let (offset, mean_path_delay) = follower.lock(|cell| {
            let f = cell.borrow();
            (f.offset(), f.mean_path_delay())
        });
        if let Some(offset) = offset {
            match mean_path_delay {
                Some(mean_path_delay) => println!(
                    "listener: time sync — offset {offset}us, mean path delay {mean_path_delay}us"
                ),
                None => println!("listener: time sync — offset {offset}us (Sync-based only)"),
            }
        }
    }
    println!("listener: done");
    std::process::exit(0);
}

#[embassy_executor::task]
async fn delay_request_task(
    follower: &'static SharedTimeSyncFollower<NoopRawMutex>,
    sender: Sender,
    clock: DemoClock,
) -> ! {
    run_periodic_delay_requests(
        follower,
        sender,
        embassy_time::Delay,
        DELAY_REQUEST_PERIOD_MS,
        clock,
    )
    .await
}

#[embassy_executor::task(pool_size = 2)]
async fn protocol_runner_task(runner: Runner) -> ! {
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

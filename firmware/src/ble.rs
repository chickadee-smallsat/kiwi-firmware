//! Bluetooth LE transport: advertises the device under its ident and streams
//! the same measurement packets that are broadcast over WiFi as GATT
//! notifications. No pairing or bonding: any central may connect, and up to
//! [`CONNECTIONS_MAX`] centrals can be connected at the same time.

// trouble-host 0.6 uses embassy-sync 0.7 and its GATT macros refer to it as
// `embassy_sync`; shadow the crate's embassy-sync 0.8 in this module.
#![allow(clippy::needless_borrows_for_generic_args)]
use embassy_sync_07 as embassy_sync;

use core::sync::atomic::{AtomicUsize, Ordering};

use ::embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex,
    pubsub::{PubSubChannel, Subscriber, WaitResult},
    signal::Signal,
};
use defmt::{debug, info, warn};
use embassy_executor::Spawner;
use embassy_futures::select::{Either3, select, select3};
use embassy_time::{Duration, Instant, Ticker, Timer, with_timeout};
use kiwi_measurements::{CommonMeasurement, SINGLE_MEASUREMENT_SIZE, SingleMeasurement};
use static_cell::StaticCell;
use trouble_host::prelude::*;

use crate::{bootsel::BOOTSEL_PRESSED, wifi::Housekeeping};

/// BLE controller backed by the CYW43 Bluetooth core
pub type BleController = ExternalController<cyw43::bluetooth::BtDriver<'static>, 10>;
type BleStack = Stack<'static, BleController, DefaultPacketPool>;
pub type BlePeripheral = Peripheral<'static, BleController, DefaultPacketPool>;

/// Max number of simultaneous BLE connections
pub const CONNECTIONS_MAX: usize = 4;
/// Max number of L2CAP channels (signalling + ATT per connection)
const L2CAP_CHANNELS_MAX: usize = 2 * CONNECTIONS_MAX;

/// Most measurements packed into one notification. The packet pool MTU (128)
/// gives an ATT MTU of 124, i.e. 121 bytes of notification payload.
const BATCH_MAX: usize = 5;
/// Largest notification payload
const NOTIFY_LEN: usize = BATCH_MAX * SINGLE_MEASUREMENT_SIZE;

/// Kiwi data service UUID (128-bit, little-endian for advertising data)
const KIWI_SERVICE_UUID: u128 = 0x6b697769_0001_4a6e_8f3b_2c9d5e7a1b00;

/// GATT server
#[gatt_server(connections_max = CONNECTIONS_MAX)]
pub struct Server {
    kiwi: KiwiService,
}

/// Kiwi data service
#[gatt_service(uuid = "6b697769-0001-4a6e-8f3b-2c9d5e7a1b00")]
struct KiwiService {
    /// Measurement stream: every packet the device broadcasts over WiFi is sent
    /// on this characteristic. Each notification carries one or more
    /// back-to-back `SINGLE_MEASUREMENT_SIZE`-byte measurements (up to
    /// [`BATCH_MAX`], as many as fit the ATT MTU, which must be >= 27).
    #[characteristic(uuid = "6b697769-0002-4a6e-8f3b-2c9d5e7a1b00", read, notify)]
    measurement: heapless::Vec<u8, NOTIFY_LEN>,
}

/// Everything needed to run the BLE side once the host is up
pub struct Ble {
    pub stack: &'static BleStack,
    pub peripheral: BlePeripheral,
    pub server: &'static Server<'static>,
    pub name: &'static str,
    pub spawner: Spawner,
}

/// Set up the BLE host stack and spawn its runner.
///
/// `mac` is the CYW43 WiFi MAC; it is turned into a static random BLE address so
/// the device keeps the same address across reboots.
pub fn init(
    spawner: Spawner,
    bt_dev: cyw43::bluetooth::BtDriver<'static>,
    mac: [u8; 6],
    ident: &heapless::String<12>,
) -> Ble {
    static RESOURCES: StaticCell<
        HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX>,
    > = StaticCell::new();
    static STACK: StaticCell<BleStack> = StaticCell::new();
    static NAME: StaticCell<heapless::String<12>> = StaticCell::new();
    static SERVER: StaticCell<Server<'static>> = StaticCell::new();

    // BLE addresses are little-endian; a static random address needs the two
    // most significant bits set.
    let mut addr = mac;
    addr.reverse();
    addr[5] |= 0xC0;
    let address = Address::random(addr);
    info!("BLE address: {:?}", address);

    let controller: BleController = ExternalController::new(bt_dev);
    let stack: &'static BleStack = STACK.init(
        trouble_host::new(controller, RESOURCES.init(HostResources::new()))
            .set_random_address(address),
    );
    let Host {
        peripheral, runner, ..
    } = stack.build();
    spawner.spawn(defmt::unwrap!(ble_runner_task(runner)));

    let name = NAME.init(ident.clone()).as_str();
    let server = SERVER.init(defmt::unwrap!(Server::new_with_config(
        GapConfig::Peripheral(PeripheralConfig {
            name,
            appearance: &appearance::sensor::GENERIC_SENSOR,
        })
    )));
    Ble {
        stack,
        peripheral,
        server,
        name,
        spawner,
    }
}

#[embassy_executor::task]
async fn ble_runner_task(mut runner: Runner<'static, BleController, DefaultPacketPool>) -> ! {
    loop {
        if let Err(e) = runner.run().await {
            warn!("BLE host runner error: {:?}", defmt::Debug2Format(&e));
            Timer::after_millis(100).await;
        }
    }
}

/// Message fanned out to every connection task
#[derive(Clone)]
enum BleMsg {
    /// A measurement packet to notify
    Data([u8; SINGLE_MEASUREMENT_SIZE]),
    /// BLE mode is ending: disconnect and exit
    Stop,
}

/// Depth of the per-connection measurement queue. A central that falls further
/// behind than this loses the oldest packets (only that central is affected).
const FANOUT_DEPTH: usize = 16;

type Fanout = PubSubChannel<CriticalSectionRawMutex, BleMsg, FANOUT_DEPTH, CONNECTIONS_MAX, 1>;
type FanoutSubscriber =
    Subscriber<'static, CriticalSectionRawMutex, BleMsg, FANOUT_DEPTH, CONNECTIONS_MAX, 1>;

/// Measurement fan-out from the radio task to the connection tasks
static FANOUT: Fanout = PubSubChannel::new();
/// Number of live connection tasks
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
/// Signalled whenever a connection task exits
static CONN_CLOSED: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Run in BLE mode until the BOOTSEL button is pressed. Up to
/// [`CONNECTIONS_MAX`] centrals may be connected at once; the device keeps
/// advertising while there is room for another one, and every measurement is
/// notified to all connected centrals.
pub async fn run(ble: &mut Ble, hk: &Housekeeping<'_>) {
    let Ble {
        stack,
        peripheral,
        server,
        name,
        spawner,
    } = ble;
    let (stack, server): (&'static BleStack, &'static Server<'static>) = (stack, server);
    let publisher = FANOUT.immediate_publisher();

    // Accept connections and hand each one to its own task
    let accept_loop = async {
        loop {
            while ACTIVE.load(Ordering::Acquire) >= CONNECTIONS_MAX {
                CONN_CLOSED.wait().await;
            }
            info!(
                "[ble] advertising as '{}' ({}/{} connected)",
                name,
                ACTIVE.load(Ordering::Relaxed),
                CONNECTIONS_MAX
            );
            let conn = match advertise(name, peripheral, server).await {
                Ok(conn) => conn,
                Err(e) => {
                    warn!("[ble] advertising failed: {:?}", defmt::Debug2Format(&e));
                    Timer::after_secs(1).await;
                    continue;
                }
            };
            let Ok(sub) = FANOUT.subscriber() else {
                warn!("[ble] no free subscriber slot, dropping connection");
                conn.raw().disconnect();
                continue;
            };
            ACTIVE.fetch_add(1, Ordering::AcqRel);
            match conn_task(conn, sub, stack, server) {
                Ok(token) => spawner.spawn(token),
                Err(e) => {
                    // The connection is dropped along with the task arguments
                    warn!("[ble] failed to spawn connection task: {:?}", e);
                    ACTIVE.fetch_sub(1, Ordering::AcqRel);
                }
            }
        }
    };

    // Fan measurements out to the connection tasks until BOOTSEL is pressed
    let fanout_loop = async {
        loop {
            hk.watchdog.feed().await;
            match select3(
                hk.receiver.receive(),
                hk.data_rate_request(),
                BOOTSEL_PRESSED.wait(),
            )
            .await
            {
                Either3::First(msg) => {
                    let connected = ACTIVE.load(Ordering::Relaxed);
                    if connected == 0 {
                        continue; // nobody to send to
                    }
                    let bytes: [u8; SINGLE_MEASUREMENT_SIZE] = msg.into();
                    if let Some((rate, unit, pkt)) = hk.dr.update(bytes.len()) {
                        info!(
                            "Data rate: {} {}, Packet rate: {}/s, BLE connections: {}",
                            rate, unit, pkt, connected
                        );
                    }
                    publisher.publish_immediate(BleMsg::Data(bytes));
                }
                Either3::Second(true) => hk.reply.send(hk.dr.peek()).await,
                Either3::Second(false) => {}
                Either3::Third(()) => return,
            }
        }
    };

    // `accept_loop` never returns; dropping it stops advertising
    select(accept_loop, fanout_loop).await;

    // Tell every connection to disconnect and wait (briefly) for them to go
    publisher.publish_immediate(BleMsg::Stop);
    let drained = hk
        .run(with_timeout(Duration::from_secs(2), async {
            while ACTIVE.load(Ordering::Acquire) > 0 {
                CONN_CLOSED.wait().await;
            }
        }))
        .await;
    if drained.is_err() {
        warn!(
            "[ble] {} connection(s) still open after stop",
            ACTIVE.load(Ordering::Relaxed)
        );
    }
}

/// Serve one central: answer GATT requests, forward measurements as
/// notifications and send it its own RSSI every couple of seconds.
#[embassy_executor::task(pool_size = CONNECTIONS_MAX)]
async fn conn_task(
    conn: GattConnection<'static, 'static, DefaultPacketPool>,
    mut sub: FanoutSubscriber,
    stack: &'static BleStack,
    server: &'static Server<'static>,
) {
    let handle = conn.raw().handle().raw();
    info!(
        "[ble {}] connected, ATT MTU {}",
        handle,
        conn.raw().att_mtu()
    );
    // trouble-host remembers a peer's subscriptions across connections, so a
    // returning central would be notified from the first instant: before it
    // has even exchanged MTUs. At 23-byte MTU that floods the host's outbound
    // queue, the MTU exchange response gets dropped, and the central stalls
    // until it gives up. Without bonding the spec (Core Vol 3 Part G 3.3.3.3)
    // resets subscriptions on every connection anyway, so do that; centrals
    // subscribe again after service discovery.
    if let Some(table) = server.get_cccd_table(conn.raw()) {
        let cleared = table.inner().map(|(h, _)| (h, CCCD::from(0u16)));
        server.set_cccd_table(conn.raw(), CccdTable::new(cleared));
    }
    let measurement = &server.kiwi.measurement;
    let mut rssi_ticker = Ticker::every(Duration::from_secs(2));
    loop {
        match select3(conn.next(), sub.next_message(), rssi_ticker.next()).await {
            Either3::First(GattConnectionEvent::Disconnected { reason }) => {
                info!("[ble {}] disconnected: {:?}", handle, reason);
                break;
            }
            Either3::First(GattConnectionEvent::Gatt { event }) => match event.accept() {
                Ok(reply) => reply.send().await,
                Err(e) => warn!("[ble {}] error sending GATT response: {:?}", handle, e),
            },
            Either3::First(_) => {}
            Either3::Second(WaitResult::Message(BleMsg::Data(bytes))) => {
                // Pack whatever else is already queued into the same
                // notification: no added latency when the link keeps up, and
                // far fewer packets when it does not.
                let per_notify =
                    (conn.raw().att_mtu() as usize).saturating_sub(3) / SINGLE_MEASUREMENT_SIZE;
                let per_notify = per_notify.clamp(1, BATCH_MAX);
                let mut batch = heapless::Vec::<u8, NOTIFY_LEN>::new();
                let _ = batch.extend_from_slice(&bytes);
                let mut stop = false;
                let mut lagged = 0;
                while batch.len() < per_notify * SINGLE_MEASUREMENT_SIZE {
                    match sub.try_next_message() {
                        Some(WaitResult::Message(BleMsg::Data(bytes))) => {
                            let _ = batch.extend_from_slice(&bytes);
                        }
                        Some(WaitResult::Message(BleMsg::Stop)) => {
                            stop = true;
                            break;
                        }
                        Some(WaitResult::Lagged(n)) => lagged += n,
                        None => break,
                    }
                }
                if lagged > 0 {
                    warn!(
                        "[ble {}] central too slow, dropped {} packets",
                        handle, lagged
                    );
                }
                if let Err(e) = measurement.notify(&conn, &batch).await {
                    warn!("[ble {}] failed to notify measurement: {:?}", handle, e);
                }
                if stop {
                    conn.raw().disconnect();
                    break;
                }
            }
            Either3::Second(WaitResult::Message(BleMsg::Stop)) => {
                conn.raw().disconnect();
                break;
            }
            Either3::Second(WaitResult::Lagged(n)) => {
                warn!("[ble {}] central too slow, dropped {} packets", handle, n);
            }
            Either3::Third(()) => {
                if let Ok(rssi) = conn.raw().rssi(stack).await {
                    debug!("[ble {}] RSSI: {}", handle, rssi);
                    let rssi: [u8; SINGLE_MEASUREMENT_SIZE] = SingleMeasurement {
                        measurement: CommonMeasurement::Rssi(true, rssi as f32),
                        timestamp: Instant::now().as_ticks(),
                    }
                    .into();
                    let mut rssi_vec = heapless::Vec::<u8, NOTIFY_LEN>::new();
                    let _ = rssi_vec.extend_from_slice(&rssi);
                    let rssi = rssi_vec;
                    if let Err(e) = measurement.notify(&conn, &rssi).await {
                        warn!("[ble {}] failed to notify RSSI: {:?}", handle, e);
                    }
                }
            }
        }
    }
    drop(conn);
    ACTIVE.fetch_sub(1, Ordering::AcqRel);
    CONN_CLOSED.signal(());
}

/// Start connectable advertising and wait for a central to connect.
async fn advertise<'values, 'server>(
    name: &'values str,
    peripheral: &mut Peripheral<'values, BleController, DefaultPacketPool>,
    server: &'server Server<'values>,
) -> Result<
    GattConnection<'values, 'server, DefaultPacketPool>,
    BleHostError<cyw43::bluetooth::Error>,
> {
    // The service UUID goes into the advertisement itself so centrals can filter
    // on it even when scanning passively. That leaves room for at most
    // SHORT_NAME_MAX bytes of the name; the complete name is in the scan response.
    const SHORT_NAME_MAX: usize = 8;
    let mut short_len = name.len().min(SHORT_NAME_MAX);
    while !name.is_char_boundary(short_len) {
        short_len -= 1;
    }
    let short_name = AdStructure::ShortenedLocalName(&name.as_bytes()[..short_len]);
    let mut adv_data = [0; 31];
    let adv_len = AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::ServiceUuids128(&[KIWI_SERVICE_UUID.to_le_bytes()]),
            if short_len == name.len() {
                AdStructure::CompleteLocalName(name.as_bytes())
            } else {
                short_name
            },
        ],
        &mut adv_data[..],
    )?;
    let mut scan_data = [0; 31];
    let scan_len = AdStructure::encode_slice(
        &[AdStructure::CompleteLocalName(name.as_bytes())],
        &mut scan_data[..],
    )?;
    // Starting advertising is a handful of HCI commands and should take
    // milliseconds. If it hangs, the host's advertising state is stuck (e.g. the
    // runner errored out while disabling a previous advertisement); report it
    // instead of looking like we are advertising when we are not.
    let advertiser = with_timeout(
        Duration::from_secs(5),
        peripheral.advertise(
            &Default::default(),
            Advertisement::ConnectableScannableUndirected {
                adv_data: &adv_data[..adv_len],
                scan_data: &scan_data[..scan_len],
            },
        ),
    )
    .await
    .map_err(|_| BleHostError::BleHost(Error::Timeout))??;
    debug!("[ble] advertising started");
    let conn = advertiser.accept().await?.with_attribute_server(server)?;
    Ok(conn)
}

//! Radio task: owns the CYW43 and streams measurements over either WiFi (UDP
//! broadcast) or Bluetooth LE (GATT notifications). The device boots in WiFi
//! mode (or BLE mode with the `ble-default` feature); each press of the BOOTSEL
//! button switches between the two.

use core::{
    future::Future,
    pin::pin,
    str::FromStr,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use cyw43::JoinOptions;
use defmt::{debug, error, info, warn};
use embassy_task_watchdog::embassy_rp::{BoundWatchdog, TaskWatchdog};
use heapless::String;
use kiwi_measurements::{CommonMeasurement, SINGLE_MEASUREMENT_SIZE, SingleMeasurement};
use portable_atomic::AtomicU64;

use cyw43_pio::{PioSpi, RM2_CLOCK_DIVIDER};

use embassy_executor::Spawner;
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_net::{
    Config, ConfigV4, DhcpConfig, IpAddress, Ipv4Address, Ipv4Cidr, StackResources, StaticConfigV4,
    udp::{PacketMetadata, SendError, UdpSocket},
};
use embassy_rp::{
    clocks::RoscRng,
    dma::Channel as DmaChannel,
    gpio::{Level, Output},
    pio::Pio,
};
use embassy_time::{Duration, Instant, Ticker, Timer, with_timeout};
use static_cell::StaticCell;

use device_blob::{DeviceConfig, WifiConf, load_device_config};

use crate::{
    DEFAULT_DEVICE_NAME, DEFAULT_WIFI_HOST_AP, DataRateReplySender, DataRateRequestReceiver,
    MeasurementReceiver, MeasurementSender, ble,
    bootsel::BOOTSEL_PRESSED,
    fw_consts::WIFI_CREDS_ADDR,
    resources::{IrqHandlers, WifiPins, WifiSpi},
};

const DATA_RATE_MS: u32 = 2000; // Update data rate every 2 seconds
const TX_BUFFER_LEN: usize = 512; // TX buffer length for UDP socket
const UDP_PORT: u16 = 8099; // UDP port measurements are broadcast on
const WIFI_AP_CHANNEL: u8 = 5; // Channel used when running as an access point
const CLIENT_CONNECT_TIMEOUT_SECS: u64 = 60;
/// How long to try joining the configured network (incl. DHCP) before falling
/// back to an open AP named after the device ident.
const CLIENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(CLIENT_CONNECT_TIMEOUT_SECS);
/// Watchdog budget for the whole setup phase of `wifi_task`: the client connect
/// attempt plus headroom for chip init, starting the fallback AP and socket setup.
const WIFI_SETUP_TIMEOUT: Duration = Duration::from_secs(CLIENT_CONNECT_TIMEOUT_SECS + 30);
/// Longest time the radio loops wait before feeding the watchdog again
/// (must stay well below the 2 s task timeout).
const FEED_PERIOD: Duration = Duration::from_millis(500);

/// Static IPv4 configuration used whenever the device runs as an access point
fn ap_ipv4_config() -> StaticConfigV4 {
    let addr = Ipv4Address::new(169, 254, 1, 1);
    StaticConfigV4 {
        address: Ipv4Cidr::new(addr, 16),
        dns_servers: Default::default(),
        gateway: Some(addr),
    }
}

// Signal to USB task that WiFi is ready
pub(crate) static WIFI_READY: AtomicBool = AtomicBool::new(false);

/// How the WiFi side ended up after [`start_wifi`]
#[derive(Clone, Copy, PartialEq, Eq, defmt::Format)]
enum WifiMode {
    /// Joined the configured network
    Client,
    /// Running an access point (configured, or fallback after a failed join)
    Ap,
}

#[embassy_task_watchdog::task(timeout = Duration::from_secs(2), setup_timeout = WIFI_SETUP_TIMEOUT)]
pub async fn wifi_task(
    watchdog: TaskWatchdog,
    spawner: Spawner,
    p: WifiPins,
    sender: MeasurementSender,
    receiver: MeasurementReceiver,
    request: DataRateRequestReceiver,
    reply: DataRateReplySender,
) -> ! {
    let wifi_nvram = cyw43::aligned_bytes!("../../cyw43-firmware/nvram_rp2040.bin");
    let (wifi_firmware, wifi_controlmode, bt_firmware) = load_wifi_blobs();
    // Load WiFi credentials
    let conf = load_device_config(WIFI_CREDS_ADDR);
    match conf {
        Ok(ref conf) => debug!("Device credentials loaded from flash: {:?}", conf),
        Err(e) => warn!(
            "Failed to load device credentials from flash: {}! Were the device credentials flashed?",
            e
        ),
    }
    // CYW43
    let wifi_pwr = Output::new(p.en, Level::Low);
    let wifi_cs = Output::new(p.cs, Level::High);
    let mut wifi_pio = Pio::new(p.pio, IrqHandlers);
    debug!("Wi-Fi PIO initialized");
    let wifi_spi = PioSpi::new(
        &mut wifi_pio.common,
        wifi_pio.sm0,
        RM2_CLOCK_DIVIDER,
        wifi_pio.irq0,
        wifi_cs,
        p.io,
        p.ck,
        DmaChannel::new(p.dma, IrqHandlers),
    );
    debug!("Wi-Fi SPI initialized");
    static WIFI_STATE: StaticCell<cyw43::State> = StaticCell::new();
    let wifi_state = WIFI_STATE.init(cyw43::State::new());
    debug!("Wi-Fi state initialized");
    let (wifi_dev, bt_dev, mut wifi_control, wifi_runner) = cyw43::new_with_bluetooth(
        wifi_state,
        wifi_pwr,
        wifi_spi,
        wifi_firmware,
        bt_firmware,
        wifi_nvram,
    )
    .await;
    info!("Wi-Fi/BT device initialized");
    spawner.spawn(defmt::unwrap!(cyw43_task(wifi_runner)));
    debug!("Wi-Fi task spawned");
    wifi_control.init(wifi_controlmode).await;
    info!("Wi-Fi control initialized");
    wifi_control
        .set_power_management(cyw43::PowerManagementMode::PowerSave)
        .await;
    debug!("Wi-Fi power management set to PowerSave");

    let conf = conf.unwrap_or(DeviceConfig {
        wifi: WifiConf::Ap(String::from_str(DEFAULT_WIFI_HOST_AP).unwrap(), None),
        ident: String::from_str(DEFAULT_DEVICE_NAME).unwrap(),
    });

    // Generate random seed
    let seed = RoscRng.next_u64();

    // Init network stack. `start_wifi` sets the IPv4 configuration for the mode
    // it ends up in.
    static RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
    let (stack, runner) = embassy_net::new(
        wifi_dev,
        Config::default(),
        RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.spawn(defmt::unwrap!(net_task(runner)));
    info!("Network stack initialized");

    // Init BLE host (idle until BLE mode is selected)
    let mac = wifi_control.address().await;
    let mut ble = ble::init(spawner, bt_dev, mac, &conf.ident);
    info!("BLE host initialized");

    static DR_COUNTER: StaticCell<DataRateCounter<DATA_RATE_MS>> = StaticCell::new();
    let dr = DR_COUNTER.init(DataRateCounter::<DATA_RATE_MS>::default()); // Update data rate every 2s
    let hk = Housekeeping {
        receiver,
        request,
        reply,
        dr,
        watchdog: &watchdog,
    };

    // Bring up the boot radio mode. `None` means BLE mode, in which WiFi is
    // only started once the BOOTSEL button switches to it.
    // Ignore button presses from chip bring-up; one pressed while joining a
    // network aborts the join and switches to BLE.
    BOOTSEL_PRESSED.reset();
    let mut wifi_mode = if cfg!(feature = "ble-default") {
        None
    } else {
        start_wifi(&mut wifi_control, stack, &conf).await
    };

    // Static allocation for UDP socket and buffers
    let mut rx_buffer = [0u8; 128];
    let mut tx_buffer = [0u8; TX_BUFFER_LEN];
    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];

    let mut socket = UdpSocket::new(
        stack,
        &mut rx_meta,
        &mut rx_buffer,
        &mut tx_meta,
        &mut tx_buffer,
    );
    let now = Instant::now();
    Timer::after_secs(1).await; // Wait a moment for the network stack to stabilize before binding the socket
    socket.bind(UDP_PORT).unwrap();
    debug!(
        "WiFi UDP socket bound to port {} in {}",
        UDP_PORT,
        now.elapsed().as_millis() as f32 / 1000.0
    );

    WIFI_READY.store(true, Ordering::Relaxed);
    // Spawn a task to broadcast the device identity every 15 seconds
    spawner.spawn(defmt::unwrap!(wifi_ident_bcast(conf.ident.clone(), sender)));
    info!("Radio is ready");
    // Clear any pending messages in the measurement receiver to avoid sending stale data after WiFi startup
    receiver.clear();
    // Ignore button presses from before the radio was up
    BOOTSEL_PRESSED.reset();
    loop {
        wifi_mode = match wifi_mode {
            // WiFi mode: stream over UDP until the button is pressed
            Some(mode) => {
                wifi_stream(&hk, &mut wifi_control, &socket, mode).await;
                info!("Switching to Bluetooth mode");
                hk.run(stop_wifi(&mut wifi_control, mode)).await;
                None
            }
            // BLE mode: advertise / stream until the button is pressed
            None => {
                ble::run(&mut ble, &hk).await;
                info!("Switching to WiFi mode");
                hk.run(start_wifi(&mut wifi_control, stack, &conf)).await
            }
        };
        BOOTSEL_PRESSED.reset();
    }
}

/// Things that must keep happening no matter what the radio is doing: feeding
/// the watchdog, draining the measurement channel (sensor tasks block when it is
/// full) and answering data rate requests from the USB task.
pub(crate) struct Housekeeping<'a> {
    pub receiver: MeasurementReceiver,
    pub request: DataRateRequestReceiver,
    pub reply: DataRateReplySender,
    pub dr: &'static DataRateCounter<DATA_RATE_MS>,
    pub watchdog: &'a BoundWatchdog<'static>,
}

impl Housekeeping<'_> {
    /// Waits for a data rate request for at most [`FEED_PERIOD`]. Returns `true`
    /// if a request arrived and must be answered.
    pub async fn data_rate_request(&self) -> bool {
        matches!(
            select(self.request.receive(), Timer::after(FEED_PERIOD)).await,
            Either::First(())
        )
    }

    /// Drives `fut` to completion while feeding the watchdog, discarding
    /// measurements (there is nowhere to send them) and answering data rate
    /// requests.
    pub async fn run<F: Future>(&self, fut: F) -> F::Output {
        let mut fut = pin!(fut);
        loop {
            self.watchdog.feed().await;
            match select3(&mut fut, self.receiver.receive(), self.data_rate_request()).await {
                Either3::First(out) => return out,
                Either3::Second(_) => {}
                Either3::Third(true) => self.reply.send(self.dr.peek()).await,
                Either3::Third(false) => {}
            }
        }
    }
}

/// Bring WiFi up according to `conf`. In client mode, falls back to an open AP
/// named after the device ident if the network can't be joined in time.
///
/// Returns `None` if the BOOTSEL button was pressed while joining a network:
/// the join is abandoned and the caller should switch to BLE mode.
async fn start_wifi(
    control: &mut cyw43::Control<'_>,
    stack: embassy_net::Stack<'_>,
    conf: &DeviceConfig,
) -> Option<WifiMode> {
    match &conf.wifi {
        WifiConf::Ap(ssid, password) => {
            stack.set_config_v4(ConfigV4::Static(ap_ipv4_config()));
            debug!("Starting WiFi in Access Point mode: SSID='{}'", ssid);
            if let Some(pw) = &password {
                debug!("Access Point password: '{}'", pw);
                control.start_ap_wpa2(ssid, pw, WIFI_AP_CHANNEL).await;
            } else {
                control.start_ap_open(ssid, WIFI_AP_CHANNEL).await;
                debug!("Access Point has no password");
            }
            info!("Access point started");
            Some(WifiMode::Ap)
        }
        WifiConf::Client(ssid, password) => {
            stack.set_config_v4(ConfigV4::Dhcp(DhcpConfig::default()));
            debug!("Starting WiFi in Client mode: SSID='{}'", ssid);
            let join_opts = if let Some(pw) = &password {
                debug!("Client password: '{}'", pw);
                JoinOptions::new(pw.as_bytes())
            } else {
                debug!("Access Point has no password");
                JoinOptions::new_open()
            };
            // Try to join the network and acquire a DHCP lease within the timeout.
            // WIFI_SETUP_TIMEOUT leaves headroom on top of this for the fallback path.
            let connect = async {
                while let Err(e) = control.join(ssid, join_opts.clone()).await {
                    debug!("Failed to join WiFi network ({:?}). Retrying...", e);
                    Timer::after_secs(1).await;
                }
                stack.wait_link_up().await;
                info!("Joined WiFi network");
                stack.wait_config_up().await;
                info!("Network configuration acquired");
            };
            let joined = select(
                with_timeout(CLIENT_CONNECT_TIMEOUT, connect),
                BOOTSEL_PRESSED.wait(),
            )
            .await;
            let joined = match joined {
                Either::First(joined) => joined,
                Either::Second(()) => {
                    info!(
                        "Join of '{}' aborted by BOOTSEL, switching to Bluetooth mode",
                        ssid
                    );
                    control.leave().await;
                    return None;
                }
            };
            Some(match joined {
                Ok(()) => {
                    if let Some(config) = stack.config_v4() {
                        debug!(
                            "IP Address: {}, Gateway: {:?}, DNS Servers: {:?}",
                            config.address, config.gateway, config.dns_servers
                        )
                    }
                    WifiMode::Client
                }
                Err(_) => {
                    warn!(
                        "Could not connect to '{}' within {} s, falling back to open AP '{}'",
                        ssid,
                        CLIENT_CONNECT_TIMEOUT.as_secs(),
                        conf.ident
                    );
                    control.leave().await;
                    // Switch the network stack from DHCP to the static AP address
                    stack.set_config_v4(ConfigV4::Static(ap_ipv4_config()));
                    control.start_ap_open(&conf.ident, WIFI_AP_CHANNEL).await;
                    info!("Fallback access point '{}' started", conf.ident);
                    WifiMode::Ap
                }
            })
        }
    }
}

/// Take WiFi down before switching to BLE
async fn stop_wifi(control: &mut cyw43::Control<'_>, mode: WifiMode) {
    match mode {
        WifiMode::Client => control.leave().await,
        WifiMode::Ap => control.close_ap().await,
    }
    info!("WiFi stopped");
}

/// Stream measurements over UDP broadcast until the BOOTSEL button is pressed.
async fn wifi_stream(
    hk: &Housekeeping<'_>,
    control: &mut cyw43::Control<'_>,
    socket: &UdpSocket<'_>,
    mode: WifiMode,
) {
    let remote_endpoint = (IpAddress::v4(255, 255, 255, 255), UDP_PORT);
    loop {
        hk.watchdog.feed().await; // Feed the watchdog at the start of each loop iteration
        let msg = match select3(
            hk.receiver.receive(),
            hk.data_rate_request(),
            BOOTSEL_PRESSED.wait(),
        )
        .await
        {
            Either3::First(msg) => msg,
            Either3::Second(true) => {
                hk.reply.send(hk.dr.peek()).await;
                continue;
            }
            Either3::Second(false) => continue,
            Either3::Third(()) => return,
        };
        let bytes: [u8; SINGLE_MEASUREMENT_SIZE] = msg.into();
        if let Some((rate, unit, pkt)) = hk.dr.update(bytes.len()) {
            let rssi = if mode == WifiMode::Client {
                Some(control.get_rssi().await)
            } else {
                None
            };
            info!(
                "Data rate: {} {}, Packet rate: {}/s, RSSI: {:?}",
                rate, unit, pkt, rssi,
            );
            if let Some(rssi) = rssi {
                let rssi: [u8; _] = SingleMeasurement {
                    measurement: CommonMeasurement::Rssi(true, rssi as f32),
                    timestamp: Instant::now().as_ticks(),
                }
                .into();
                if let Err(e) = socket.send_to(&rssi, remote_endpoint).await {
                    error!("Failed to send RSSI UDP packet: {:?}", e);
                }
            }
        }
        if let Err(e) = socket.send_to(&bytes, remote_endpoint).await {
            let reason = Some(match e {
                SendError::NoRoute => {
                    error!("Failed to send UDP packet: No route to destination");
                    String::from_str("NoRouteToHost").unwrap()
                }
                SendError::SocketNotBound => {
                    error!("Failed to send UDP packet: Socket is not bound to a local port");
                    String::from_str("SocketNotBound").unwrap()
                }
                SendError::PacketTooLarge => {
                    warn!("Failed to send UDP packet: Packet size exceeds buffer capacity");
                    continue;
                }
            });
            Timer::after_secs(1).await; // Wait a moment before triggering reset to allow logs to flush
            hk.watchdog.trigger_reset(reason).await;
        };
    }
}

#[embassy_executor::task]
async fn wifi_ident_bcast(ident: String<12>, sender: MeasurementSender) {
    let mut ticker = Ticker::every(Duration::from_secs(15)); // Broadcast the device identity every 15 seconds
    loop {
        let measurement = kiwi_measurements::CommonMeasurement::Id(ident.clone());
        sender
            .send(SingleMeasurement {
                measurement,
                timestamp: Instant::now().as_ticks(),
            })
            .await;
        ticker.next().await; // Broadcast the device identity every 15 seconds
    }
}

#[embassy_executor::task]
async fn cyw43_task(runner: cyw43::Runner<'static, cyw43::SpiBus<Output<'static>, WifiSpi>>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, cyw43::NetDriver<'static>>) -> ! {
    runner.run().await
}

pub struct DataRateCounter<const RATE: u32> {
    bytes: AtomicUsize,
    count: AtomicUsize,
    start: AtomicU64,
}

impl<const RATE: u32> Default for DataRateCounter<RATE> {
    fn default() -> Self {
        Self {
            bytes: AtomicUsize::new(0),
            count: AtomicUsize::new(0),
            start: AtomicU64::new(Instant::now().as_ticks()),
        }
    }
}

impl<const RATE: u32> DataRateCounter<RATE> {
    pub fn update(&self, bytes: usize) -> Option<(f32, &str, f32)> {
        let nnow = Instant::now();
        let earlier = Instant::from_ticks(self.start.load(Ordering::Relaxed));
        let dur = nnow.duration_since(earlier).as_millis();
        self.count.fetch_add(1, Ordering::Relaxed);
        if dur > RATE as u64 {
            let dur = dur as f32 / 1_000.0;
            self.start.store(nnow.as_ticks(), Ordering::Relaxed);
            let sent_bytes = self.bytes.swap(0, Ordering::Relaxed) + bytes;
            if sent_bytes == 0 {
                None
            } else {
                let bytes = (sent_bytes * 8) as f32;
                Some({
                    let pkt_rate = self.count.swap(0, Ordering::Relaxed) as f32 / dur;
                    match bytes {
                        b if b >= 1024.0 * 1024.0 => {
                            (bytes / 1024.0 / 1024.0 / dur, "mbps", pkt_rate)
                        }
                        b if b >= 1024.0 => (bytes / 1024.0 / dur, "kbps", pkt_rate),
                        b => (b / dur, "bps", pkt_rate),
                    }
                })
            }
        } else {
            self.bytes.fetch_add(bytes, Ordering::Relaxed);
            None
        }
    }

    pub fn peek(&self) -> Option<(f32, &str, f32)> {
        let nnow = Instant::now();
        let dur = nnow
            .duration_since(Instant::from_ticks(self.start.load(Ordering::Relaxed)))
            .as_millis() as f32
            / 1_000.0;
        let sent_bytes = self.bytes.load(Ordering::Relaxed);
        if sent_bytes == 0 {
            None
        } else {
            let bytes = (sent_bytes * 8) as f32;
            Some({
                let pkt_rate = self.count.load(Ordering::Relaxed) as f32 / dur;
                match bytes {
                    b if b >= 1024.0 * 1024.0 => (bytes / 1024.0 / 1024.0 / dur, "mbps", pkt_rate),
                    b if b >= 1024.0 => (bytes / 1024.0 / dur, "kbps", pkt_rate),
                    b => (b / dur, "bps", pkt_rate),
                }
            })
        }
    }
}

type WiFiBlobRet = (
    &'static cyw43::Aligned<cyw43::A4, [u8]>,
    &'static cyw43::Aligned<cyw43::A4, [u8]>,
    &'static cyw43::Aligned<cyw43::A4, [u8]>,
);

#[cfg(not(feature = "monolithic"))]
use device_blob::load_firmware_blob;

#[cfg(not(feature = "monolithic"))]
use crate::fw_consts::{
    BT_FW_ADDR, BT_FW_CRC, BT_FW_LEN, WIFI_CLM_ADDR, WIFI_CLM_CRC, WIFI_CLM_LEN, WIFI_FW_ADDR,
    WIFI_FW_CRC, WIFI_FW_LEN,
};

#[cfg(not(feature = "monolithic"))]
/// Load WiFi and Bluetooth blobs from fixed addresses
fn load_wifi_blobs() -> WiFiBlobRet {
    let wifi_firmware = unsafe { load_firmware_blob(WIFI_FW_ADDR, WIFI_FW_LEN, WIFI_FW_CRC) }.inspect_err(|e| error!(
            "WiFi Firmware CRC did not match! Expected: 0x{:x}, Calculated: 0x{:x}. Was the WiFi firmware flashed?",
            WIFI_FW_CRC,
            e
        )).expect("WiFi firmware verification failed");
    let wifi_controlmode =
        unsafe{load_firmware_blob(WIFI_CLM_ADDR, WIFI_CLM_LEN, WIFI_CLM_CRC)}.inspect_err(|e| error!(
            "WiFi CLM CRC did not match! Expected: 0x{:x}, Calculated: 0x{:x}. Was the WiFi firmware flashed?",
            WIFI_CLM_CRC,
            e
        )).expect("WiFi CLM verification failed");
    let bt_firmware =
        unsafe{load_firmware_blob(BT_FW_ADDR, BT_FW_LEN, BT_FW_CRC)}.inspect_err(|e| error!(
            "Bluetooth firmware CRC did not match! Expected: 0x{:x}, Calculated: 0x{:x}. Was the Bluetooth firmware flashed (flash-wifi-fw.sh)?",
            BT_FW_CRC,
            e
        )).expect("Bluetooth firmware verification failed");
    debug!("Wi-Fi firmware, CLM and Bluetooth firmware verified");
    (wifi_firmware, wifi_controlmode, bt_firmware)
}

#[cfg(feature = "monolithic")]
/// Load WiFi and Bluetooth blobs from bytes baked into the firmware
fn load_wifi_blobs() -> WiFiBlobRet {
    let wifi_firmware = cyw43::aligned_bytes!("../../cyw43-firmware/43439A0.bin");
    let wifi_controlmode = cyw43::aligned_bytes!("../../cyw43-firmware/43439A0_clm.bin");
    let bt_firmware = cyw43::aligned_bytes!("../../cyw43-firmware/43439A0_btfw.bin");
    debug!("Wi-Fi firmware, CLM and Bluetooth firmware baked into firmware");
    (wifi_firmware, wifi_controlmode, bt_firmware)
}

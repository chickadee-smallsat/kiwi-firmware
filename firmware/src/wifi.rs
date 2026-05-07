use core::{
    str::FromStr,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use cyw43::JoinOptions;
use defmt::*;
use embassy_task_watchdog::embassy_rp::TaskWatchdog;
use heapless::String;
use kiwi_measurements::{SINGLE_MEASUREMENT_SIZE, SingleMeasurement};
use portable_atomic::AtomicU64;

use cyw43_pio::{PioSpi, RM2_CLOCK_DIVIDER};

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_net::{
    Config, IpAddress, Ipv4Address, Ipv4Cidr, StackResources, StaticConfigV4,
    udp::{PacketMetadata, SendError, UdpSocket},
};
use embassy_rp::{
    clocks::RoscRng,
    dma::Channel as DmaChannel,
    gpio::{Level, Output},
    pio::Pio,
};
use embassy_time::{Duration, Instant, Ticker, Timer};
use static_cell::StaticCell;

use device_blob::{DeviceConfig, WifiConf, load_device_config, load_firmware_blob};

use crate::{
    DEFAULT_DEVICE_NAME, DEFAULT_WIFI_HOST_AP, DataRateReplySender, DataRateRequestReceiver,
    MeasurementReceiver, MeasurementSender,
    resources::{IrqHandlers, WifiPins, WifiSpi},
    wifi_fw_consts::{
        WIFI_CLM_ADDR, WIFI_CLM_CRC, WIFI_CLM_LEN, WIFI_CREDS_ADDR, WIFI_FW_ADDR, WIFI_FW_CRC,
        WIFI_FW_LEN,
    },
};

const DATA_RATE_MS: u32 = 2000; // Update data rate every 2 seconds
const TX_BUFFER_LEN: usize = 512; // TX buffer length for UDP socket

// Signal to USB task that WiFi is ready
pub(crate) static WIFI_READY: AtomicBool = AtomicBool::new(false);

#[embassy_task_watchdog::task(timeout = Duration::from_secs(2), setup_timeout = Duration::from_secs(60))]
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
    info!("Wi-Fi firmware and CLM verified");
    // Load WiFi credentials
    let conf = load_device_config(WIFI_CREDS_ADDR);
    match conf {
        Ok(ref conf) => info!("Device credentials loaded from flash: {}", conf),
        Err(e) => warn!(
            "Failed to load device credentials from flash: {}! Were the device credentials flashed?",
            e
        ),
    }
    // CYW43
    let wifi_pwr = Output::new(p.en, Level::Low);
    let wifi_cs = Output::new(p.cs, Level::High);
    let mut wifi_pio = Pio::new(p.pio, IrqHandlers);
    info!("Wi-Fi PIO initialized");
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
    info!("Wi-Fi SPI initialized");
    static WIFI_STATE: StaticCell<cyw43::State> = StaticCell::new();
    let wifi_state = WIFI_STATE.init(cyw43::State::new());
    info!("Wi-Fi state initialized");
    let (wifi_dev, mut wifi_control, wifi_runner) =
        cyw43::new(wifi_state, wifi_pwr, wifi_spi, wifi_firmware, wifi_nvram).await;
    info!("Wi-Fi device initialized");
    spawner.must_spawn(cyw43_task(wifi_runner));
    info!("Wi-Fi task spawned");
    wifi_control.init(wifi_controlmode).await;
    info!("Wi-Fi control initialized");
    wifi_control
        .set_power_management(cyw43::PowerManagementMode::PowerSave)
        .await;
    info!("Wi-Fi power management set to PowerSave");

    let conf = conf.unwrap_or(DeviceConfig {
        wifi: WifiConf::Ap(String::from_str(DEFAULT_WIFI_HOST_AP).unwrap(), None),
        ident: String::from_str(DEFAULT_DEVICE_NAME).unwrap(),
    });

    // Generate random seed
    let seed = RoscRng.next_u64();

    let netconf = match &conf.wifi {
        WifiConf::Ap(_, _) => {
            let addr = Ipv4Address::new(169, 254, 1, 1);
            Config::ipv4_static(StaticConfigV4 {
                address: Ipv4Cidr::new(addr, 16),
                dns_servers: Default::default(),
                gateway: Some(addr),
            })
        }

        WifiConf::Client(_, _) => Config::dhcpv4(Default::default()),
    };

    // Init network stack
    static RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
    let (stack, runner) = embassy_net::new(
        wifi_dev,
        netconf,
        RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.must_spawn(net_task(runner));
    info!("Network stack initialized");

    // Start WiFi
    match &conf.wifi {
        WifiConf::Ap(ssid, password) => {
            info!("Starting WiFi in Access Point mode: SSID='{}'", ssid);
            if let Some(pw) = &password {
                info!("Access Point password: '{}'", pw);
                wifi_control.start_ap_wpa2(ssid, pw, 5).await;
            } else {
                wifi_control.start_ap_open(ssid, 5).await;
                info!("Access Point has no password");
            }
            info!("Access point started");
        }
        WifiConf::Client(ssid, password) => {
            info!("Starting WiFi in Client mode: SSID='{}'", ssid);
            let join_opts = if let Some(pw) = &password {
                info!("Client password: '{}'", pw);
                JoinOptions::new(pw.as_bytes())
            } else {
                info!("Access Point has no password");
                JoinOptions::new_open()
            };
            while wifi_control.join(ssid, join_opts.clone()).await.is_err() {
                info!("Failed to join WiFi network. Retrying...");
            }
            stack.wait_link_up().await;
            info!("Joined WiFi network");
            stack.wait_config_up().await;
            info!("Network configuration acquired");
            if let Some(config) = stack.config_v4() {
                info!(
                    "IP Address: {}, Gateway: {}, DNS Servers: {:?}",
                    config.address, config.gateway, config.dns_servers
                )
            }
        }
    }

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
    socket.bind(8099).unwrap();
    info!(
        "WiFi UDP socket bound to port 8099 in {}",
        now.elapsed().as_millis() as f32 / 1000.0
    );
    static DR_COUNTER: StaticCell<DataRateCounter<DATA_RATE_MS>> = StaticCell::new();
    let dr = DR_COUNTER.init(DataRateCounter::<DATA_RATE_MS>::default()); // Update data rate every 2s

    let remote_endpoint = (IpAddress::v4(255, 255, 255, 255), 8099);
    WIFI_READY.store(true, Ordering::Relaxed);
    // watchdog.feed().await; // Feed the watchdog after WiFi is ready
    // Spawn a task to broadcast the device identity every 60 seconds
    spawner.must_spawn(wifi_ident_bcast(conf.ident.clone(), sender));
    info!("Wi-Fi is ready");
    // Clear any pending messages in the measurement receiver to avoid sending stale data after WiFi startup
    receiver.clear();
    loop {
        watchdog.feed().await; // Feed the watchdog at the start of each loop iteration
        match select(receiver.receive(), request.receive()).await {
            Either::First(msg) => {
                let bytes: [u8; SINGLE_MEASUREMENT_SIZE] = msg.into();
                if let Some((rate, unit, pkt)) = dr.update(bytes.len()) {
                    info!("Data rate: {} {}, Packet rate: {}/s", rate, unit, pkt);
                }
                if let Err(e) = socket.send_to(&bytes, remote_endpoint).await {
                    let reason = Some(match e {
                        SendError::NoRoute => {
                            error!("Failed to send UDP packet: No route to destination");
                            String::from_str("NoRouteToHost").unwrap()
                        }
                        SendError::SocketNotBound => {
                            error!(
                                "Failed to send UDP packet: Socket is not bound to a local port"
                            );
                            String::from_str("SocketNotBound").unwrap()
                        }
                        SendError::PacketTooLarge => {
                            warn!("Failed to send UDP packet: Packet size exceeds buffer capacity");
                            continue;
                        }
                    });
                    Timer::after_secs(1).await; // Wait a moment before triggering reset to allow logs to flush
                    watchdog.trigger_reset(reason).await;
                };
            }
            Either::Second(_) => {
                let peek = dr.peek();
                reply.send(peek).await;
            }
            // for some reason rust-analyzer on vscode thinks the two matches are not exhaustive enough
            // even though code compiles just fine, and the compiler does not complain about exhaustiveness at all
            #[allow(unreachable_patterns)]
            // but rust-analyzer also thinks this catch-all check is redundant
            _ => {
                // This branch is needed to satisfy the exhaustiveness check, but it should never be hit
                core::unreachable!();
            }
        }
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

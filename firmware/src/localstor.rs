use core::{
    fmt::Write as _,
    sync::atomic::{AtomicBool, AtomicU16, Ordering},
};

use defmt::{error, info};
use embassy_embedded_hal::SetConfig;
use embassy_rp::{
    gpio::{Input, Level, Output, Pull},
    spi::{self, Spi},
};
use embassy_sync::{
    blocking_mutex::raw::NoopRawMutex,
    channel::{Channel, Receiver, Sender},
};
use embassy_time::{Duration, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use embedded_sdmmc::SdCard;
use static_cell::StaticCell;

use crate::{MeasurementReceiver, resources::MmcPins};

/// Shorthand for the SD card detect pin type used throughout this module.
type DetectPin = Input<'static>;

/// Shorthand for the SD card type used throughout this module.
type MemCard = SdCard<
    ExclusiveDevice<
        Spi<'static, embassy_rp::peripherals::SPI1, spi::Blocking>,
        Output<'static>,
        embedded_hal_bus::spi::NoDelay,
    >,
    embassy_time::Delay,
>;

type CardDetChannel = Channel<NoopRawMutex, (), 1>;
type CardDetSender = Sender<'static, NoopRawMutex, (), 1>;
type CardDetReceiver = Receiver<'static, NoopRawMutex, (), 1>;

/// Open directory handle on the SD card.
type CardDir<'a> = embedded_sdmmc::Directory<
    'a,
    SdCard<
        ExclusiveDevice<
            Spi<'static, embassy_rp::peripherals::SPI1, spi::Blocking>,
            Output<'static>,
            embedded_hal_bus::spi::NoDelay,
        >,
        embassy_time::Delay,
    >,
    DummyTimesource,
    4,
    4,
    1,
>;

/// Shorthand for the SD card error type used throughout this module.
type SdError = embedded_sdmmc::Error<embedded_sdmmc::SdCardError>;

/// An open file handle on the SD card.
type KiwiFile<'a> = embedded_sdmmc::File<
    'a,
    SdCard<
        ExclusiveDevice<
            Spi<'static, embassy_rp::peripherals::SPI1, spi::Blocking>,
            Output<'static>,
            embedded_hal_bus::spi::NoDelay,
        >,
        embassy_time::Delay,
    >,
    DummyTimesource,
    4,
    4,
    1,
>;

/// Set to `true` once the SD card is mounted and measurements are being written;
/// cleared on card removal or write error.
pub static MMC_READY: AtomicBool = AtomicBool::new(false);

/// Initializes the local storage task, which handles SD card detection, mounting, and writing measurements to the card.
pub fn local_storage_init(
    spawner: embassy_executor::Spawner,
    mmc: MmcPins,
    receiver: MeasurementReceiver,
) {
    static CARD_DET_CHANNEL: StaticCell<CardDetChannel> = StaticCell::new();
    let channel = CARD_DET_CHANNEL.init(Channel::new());

    let cdet = Input::new(mmc.cd, Pull::None);

    // SPI must run at ≤ 400 kHz during SD card initialisation
    let mut config = spi::Config::default();
    config.frequency = 400_000;
    let spi = Spi::new_blocking(mmc.spi, mmc.ck, mmc.dout, mmc.din, config);
    let cs = Output::new(mmc.cs, Level::High);
    let spi_dev = match ExclusiveDevice::new_no_delay(spi, cs) {
        Ok(dev) => dev,
        Err(_) => {
            error!("Failed to create SPI device");
            return;
        }
    };

    let sdcard: MemCard = SdCard::new(spi_dev, embassy_time::Delay);
    let mut config = spi::Config::default();
    config.frequency = 16_000_000;
    match sdcard
        .spi(|dev| SetConfig::set_config(dev.bus_mut(), &config))
        .ok()
    {
        Some(()) => info!("SD card SPI frequency set to 16 MHz"),
        None => error!("Failed to set SD card SPI frequency"),
    }

    spawner.spawn(card_handler(channel.receiver(), sdcard, receiver).unwrap());
    spawner.spawn(card_detector(cdet, channel.sender()).unwrap());
}

#[embassy_executor::task]
/// Waits for the SD card detect pin to go low (active-low) and sends a message
/// to the `CardDetChannel` when a card is inserted. Debounces the pin for 100 ms.
async fn card_detector(inp: DetectPin, tx: CardDetSender) {
    let mut inp = inp;
    // Fire immediately if a card is already inserted (detect pin is active-low)
    if !inp.is_high() {
        tx.send(()).await;
    }
    loop {
        inp.wait_for_any_edge().await;
        Timer::after(Duration::from_millis(100)).await; // debounce
        if !inp.is_high() {
            tx.send(()).await;
        }
    }
}

#[embassy_executor::task]
/// Waits for a card insertion message from the `CardDetChannel`, then attempts
/// to mount the SD card and write measurements to it. If the card is removed or
/// an unrecoverable error occurs, the task returns to waiting for the next card insertion.
async fn card_handler(rx: CardDetReceiver, card: MemCard, receiver: MeasurementReceiver) {
    let vol_mgr = embedded_sdmmc::VolumeManager::new(card, DummyTimesource);

    // Build the 256-byte file header once. DEVICE_ID is populated before
    // local_storage_init is called, so this is safe to do immediately.
    let file_header = build_file_header().await;

    'main: loop {
        info!("Waiting for SD card...");
        MMC_READY.store(false, Ordering::Relaxed);
        rx.receive().await;

        // Force the driver to re-detect and re-initialise the card
        vol_mgr.device(|d| {
            d.mark_card_uninit();
            match d.num_bytes() {
                Ok(b) => info!("SD card: {} MiB", b as f32 / (1 << 20) as f32),
                Err(e) => error!("Capacity read failed: {:?}", defmt::Debug2Format(&e)),
            }
            DummyTimesource
        });
        Timer::after_secs(1).await; // let the card stabilise

        // FAT32 supports at most 4 primary partitions
        'vol: for i in 0..4usize {
            let raw = match vol_mgr.open_raw_volume(embedded_sdmmc::VolumeIdx(i)) {
                Ok(v) => v,
                Err(embedded_sdmmc::Error::NoSuchVolume) => {
                    info!("No volume at index {}", i);
                    break 'vol; // no more partitions to try
                }
                Err(embedded_sdmmc::Error::FormatError(s)) => {
                    error!("Volume {} format error: {}", i, s);
                    continue 'vol; // try the next partition
                }
                Err(e) => {
                    error!("Volume {} open error: {:?}", i, defmt::Debug2Format(&e));
                    break 'vol;
                }
            };

            let mut label = heapless::String::<32>::new();
            match vol_mgr.get_root_volume_label(raw) {
                Ok(None) => {
                    let _ = label.push_str("NOLABEL");
                }
                Ok(Some(l)) => {
                    for c in core::str::from_utf8(l.name()).unwrap_or("INVALID").chars() {
                        if label.push(c).is_err() {
                            break;
                        }
                    }
                }
                Err(e) => error!(
                    "Label read error on volume {}: {:?}",
                    i,
                    defmt::Debug2Format(&e)
                ),
            }
            info!("Volume {} label: {}", i, label);

            let volume = raw.to_volume(&vol_mgr);
            let root_dir: CardDir = match volume.open_root_dir() {
                Ok(d) => d,
                Err(e) => {
                    error!("open_root_dir failed: {:?}", defmt::Debug2Format(&e));
                    continue 'main;
                }
            };

            if let Err(e) = store_data(root_dir, &receiver, &file_header).await {
                MMC_READY.store(false, Ordering::Relaxed);
                error!("store_data error: {:?}", defmt::Debug2Format(&e));
                continue 'main; // card removed or unrecoverable error
            }
        }
    }
}

/// Wraps an open FAT directory with domain-specific navigation and file-open
/// helpers for KIWI sensor data storage.
struct KiwiDir<'a>(CardDir<'a>);

impl<'a> KiwiDir<'a> {
    /// Navigate into the first available `candidate` directory, creating it if
    /// it does not exist. Skips names that are blocked by a non-directory entry
    /// (e.g. a volume label with the same name).
    fn enter_or_create(&mut self, candidates: &[&str]) -> Result<(), SdError> {
        for &candidate in candidates {
            let name = embedded_sdmmc::ShortFileName::create_from_str(candidate)
                .map_err(embedded_sdmmc::Error::FilenameError)?;
            match self.0.find_directory_entry(&name) {
                Ok(e) if e.attributes.is_directory() => {
                    self.0.change_dir(&name)?;
                    return Ok(());
                }
                Ok(_) => {
                    // Name exists but is not a directory (e.g. volume label); try next
                    info!("{} is not a directory, trying next candidate", candidate);
                    continue;
                }
                Err(embedded_sdmmc::Error::NotFound) => {
                    info!("Creating {}/", candidate);
                    self.0.make_dir_in_dir(&name)?;
                    self.0.change_dir(&name)?;
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
        }
        error!("All data directory candidates blocked");
        Err(embedded_sdmmc::Error::NotFound)
    }

    /// Navigate into the run directory for this firmware boot, creating it if
    /// needed. The run number is cached in a `static` so that card removal and
    /// reinsertion within the same boot reuses the same directory.
    fn enter_run_dir(&mut self) -> Result<(), SdError> {
        static RUN: AtomicU16 = AtomicU16::new(u16::MAX); // u16::MAX = "unassigned"

        let idx = match RUN.load(Ordering::Relaxed) {
            u16::MAX => {
                let mut idx = 1u16; // first run directory is "00001"
                self.0.iterate_dir(|e| {
                    if e.attributes.is_directory() {
                        let n = core::str::from_utf8(e.name.base_name())
                            .unwrap_or("0")
                            .parse::<u16>()
                            .unwrap_or(0);
                        if n >= idx {
                            idx = n + 1;
                        }
                    }
                })?;
                let name = embedded_sdmmc::ShortFileName::create_from_str(
                    heapless::format!(5; "{:05}", idx).unwrap().as_str(),
                )
                .map_err(embedded_sdmmc::Error::FilenameError)?;
                self.0.make_dir_in_dir(&name)?;
                RUN.store(idx, Ordering::Relaxed);
                idx
            }
            current => current,
        };

        let name = embedded_sdmmc::ShortFileName::create_from_str(
            heapless::format!(5; "{:05}", idx).unwrap().as_str(),
        )
        .map_err(embedded_sdmmc::Error::FilenameError)?;
        info!("Run directory: {:?}", defmt::Debug2Format(&name));
        self.0.change_dir(&name)
    }

    /// Open the next `.BIN` file for writing in the current directory.
    ///
    /// Scans for the highest-numbered `NNNNN.BIN` and opens a new file with a
    /// number one higher.
    fn open_next_bin_file(&self) -> Result<KiwiFile<'_>, SdError> {
        let mut idx = 0u16;
        self.0.iterate_dir(|e| {
            if !e.attributes.is_directory() && e.name.extension() == b"BIN" {
                let n = core::str::from_utf8(e.name.base_name())
                    .unwrap_or("0")
                    .parse::<u16>()
                    .unwrap_or(0);
                if n >= idx {
                    idx = n + 1;
                }
            }
        })?;
        let fname = embedded_sdmmc::ShortFileName::create_from_str(
            heapless::format!(11; "{:05}.BIN", idx).unwrap().as_str(),
        )
        .map_err(embedded_sdmmc::Error::FilenameError)?;
        info!("Opening {:?}", defmt::Debug2Format(&fname));
        self.0
            .open_file_in_dir(&fname, embedded_sdmmc::Mode::ReadWriteCreateOrTruncate)
    }
    /// Write a `README.TXT` explaining the data layout into the current directory,
    /// if it does not already exist.
    fn write_readme_if_absent(&self) -> Result<(), SdError> {
        const CONTENT: &[u8] = b"\
KIWI Sensor Data\r\n\
================\r\n\
\r\n\
Directory layout:\r\n\
  KIWI/\r\n\
    README.TXT          this file\r\n\
    00001/              firmware run 1\r\n\
      00000.BIN         first data segment\r\n\
      00001.BIN         second segment (if first exceeded 3 GiB)\r\n\
    00002/              firmware run 2 (next power-on)\r\n\
      ...\r\n\
\r\n\
File format (.BIN):\r\n\
  [0..256]  Human-readable text header (ASCII, zero-padded)\r\n\
  [256..]   SingleMeasurement records, 24 bytes each\r\n\
\r\n\
To parse: skip the first 256 bytes, then read every 24 bytes as one record.\r\n";
        let name = embedded_sdmmc::ShortFileName::create_from_str("README.TXT")
            .map_err(embedded_sdmmc::Error::FilenameError)?;
        if self.0.find_directory_entry(&name).is_ok() {
            return Ok(()); // already exists
        }
        let file = self
            .0
            .open_file_in_dir(&name, embedded_sdmmc::Mode::ReadWriteCreateOrAppend)?;
        file.write(CONTENT)?;
        file.flush()?;
        Ok(())
    }
}

/// Layout: `KIWI/<run>/NNNNN.BIN`  (falls back to `KIWIDAT/` if KIWI is blocked)
/// - `<run>` increments each firmware boot (see [`KiwiDir::enter_run_dir`]).
/// - A new `NNNNN.BIN` is opened whenever the 3 GiB per-file limit is reached.
/// - Every file starts with the 256-byte `file_header`.
async fn store_data(
    dir: CardDir<'_>,
    receiver: &MeasurementReceiver,
    file_header: &[u8; 256],
) -> Result<(), SdError> {
    let mut dir = KiwiDir(dir);
    dir.enter_or_create(&["KIWI", "KIWIDAT"])?;
    dir.write_readme_if_absent()?;
    dir.enter_run_dir()?;

    MMC_READY.store(true, Ordering::Relaxed);

    // Outer loop: each iteration owns one file segment (capped at 3 GiB)
    'file: loop {
        let file = dir.open_next_bin_file()?;
        file.write(file_header)?;
        file.flush()?;

        let mut written = 0usize;
        let mut buf = heapless::Vec::<u8, 3072>::new();

        // Inner loop: stream measurements into the current file
        loop {
            let bytes: [u8; _] = receiver.receive().await.into();

            if buf.extend_from_slice(&bytes).is_err() {
                // Buffer full — flush to disk and reset
                file.write(&buf)?;
                file.flush()?;
                written += buf.len();
                buf.clear();
                buf.extend_from_slice(&bytes).unwrap(); // SAFETY: buf just cleared; 24 B always fits
            }

            if written >= 3 * (1usize << 30) {
                // Flush the remaining buffer, then roll over to a new file
                file.write(&buf)?;
                file.flush()?;
                buf.clear();
                info!("3 GiB reached, rolling over");
                continue 'file;
            }
        }
    }
}

/// Builds a human-readable 256-byte file header stamped with the device ID.
///
/// ```text
/// KIWI Sensor Data
/// Device: <id>
/// Format version: 1
/// Header size: 256 bytes
/// Record size: 24 bytes
/// Records start at byte 256
/// ```
/// The remainder of the 256 bytes is zero-padded.
async fn build_file_header() -> [u8; 256] {
    struct SliceWriter<'a> {
        buf: &'a mut [u8],
        pos: usize,
    }
    impl core::fmt::Write for SliceWriter<'_> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let b = s.as_bytes();
            let n = b.len().min(self.buf.len().saturating_sub(self.pos));
            self.buf[self.pos..self.pos + n].copy_from_slice(&b[..n]);
            self.pos += n;
            Ok(())
        }
    }

    let mut header = [0u8; 256];
    let dev_id = crate::DEVICE_ID.lock().await;
    let mut w = SliceWriter {
        buf: &mut header,
        pos: 0,
    };
    let _ = write!(
        w,
        "KIWI Sensor Data\nDevice: {}\nFormat version: 1\nHeader size: 256 bytes\nRecord size: 24 bytes\nRecords start at byte 256\n",
        dev_id.as_str()
    );
    header
}

/// Timestamp stub
struct DummyTimesource;

impl embedded_sdmmc::TimeSource for DummyTimesource {
    fn get_timestamp(&self) -> embedded_sdmmc::Timestamp {
        embedded_sdmmc::Timestamp {
            year_since_1970: crate::fw_consts::BUILD_YEAR_SINCE_1970,
            zero_indexed_month: crate::fw_consts::BUILD_ZERO_INDEXED_MONTH,
            zero_indexed_day: crate::fw_consts::BUILD_ZERO_INDEXED_DAY,
            hours: 0,
            minutes: 0,
            seconds: 0,
        }
    }
}

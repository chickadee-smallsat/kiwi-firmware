#![no_std]
#![deny(missing_docs)]
//! This crate provides utilities for loading device configuration and firmware blobs from flash memory.
//! It defines the structure of the device configuration, including WiFi settings and a device identifier,
//! and provides functions to serialize/deserialize this configuration to/from a specific flash layout with CRC16 checksums.
//! It also provides a function to load firmware blobs from flash, verifying their integrity using CRC16 checksums.
use core::str::FromStr as _;

use aligned::{A4, Aligned};
#[cfg(feature = "defmt")]
use defmt::{Formatter, error, warn};

// A replacement for the defmt logging macros, when defmt is not provided
#[cfg(not(feature = "defmt"))]
mod log_impl {
    #![allow(unused_macros)]
    #![allow(unused_imports)]
    // Macros are defined as _ to avoid conflicts with built-in attribute
    // names
    macro_rules! _trace {
        ($($arg:tt)*) => {};
    }
    macro_rules! _debug {
        ($($arg:tt)*) => {};
    }
    macro_rules! _info {
        ($($arg:tt)*) => {};
    }
    macro_rules! _warn {
        ($($arg:tt)*) => {};
    }
    macro_rules! _error {
        ($($arg:tt)*) => {};
    }
    pub(crate) use _debug as debug;
    pub(crate) use _error as error;
    pub(crate) use _info as info;
    pub(crate) use _trace as trace;
    pub(crate) use _warn as warn;
}
#[cfg(not(feature = "defmt"))]
use log_impl::*;

use heapless::String;

type CrcState = crc16::State<crc16::XMODEM>;

/// Size of the flash block used to store device configuration and WiFi credentials.
pub const BLOCK_SIZE: usize = 0x1000; // 4KB flash page size

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
/// Device configuration
pub struct DeviceConfig {
    /// WiFi configuration (AP or Client)
    pub wifi: WifiConf,
    /// Device identifier string
    pub ident: String<12>,
}

/// WiFi configuration: Access Point or Client
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WifiConf {
    /// Access Point mode with SSID and optional password
    Ap(String<32>, Option<String<64>>),
    /// Client mode with SSID and optional password
    Client(String<32>, Option<String<64>>),
}

impl TryFrom<(u32, &str, Option<&str>)> for WifiConf {
    type Error = ();
    fn try_from(inp: (u32, &str, Option<&str>)) -> Result<Self, Self::Error> {
        let (value, ssid, password) = inp;
        let ssid = String::from_str(ssid).map_err(|_| ())?;
        let password = if let Some(pw) = password {
            Some(String::from_str(pw).map_err(|_| ())?)
        } else {
            None
        };
        match value {
            0 => Ok(WifiConf::Ap(ssid, password)),
            1 => Ok(WifiConf::Client(ssid, password)),
            _ => {
                error!(
                    "Invalid WiFi mode: {}. Must be 0 (AP) or 1 (WPA2 Personal)",
                    value
                );
                Err(())
            }
        }
    }
}

impl WifiConf {
    fn as_u32(&self) -> u32 {
        match self {
            WifiConf::Ap(_, _) => 0,
            WifiConf::Client(_, _) => 1,
        }
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for WifiConf {
    fn format(&self, fmt: Formatter) {
        match self {
            WifiConf::Ap(ssid, Some(pw)) => {
                defmt::write!(fmt, "AP Mode: SSID='{}', Password='{}'", ssid, pw)
            }
            WifiConf::Ap(ssid, None) => defmt::write!(fmt, "AP Mode: SSID='{}', No Password", ssid),
            WifiConf::Client(ssid, Some(pw)) => {
                defmt::write!(fmt, "Client Mode: SSID='{}', Password='{}'", ssid, pw)
            }
            WifiConf::Client(ssid, None) => {
                defmt::write!(fmt, "Client Mode: SSID='{}', No Password", ssid)
            }
        }
    }
}

/// Load device configuration from flash memory, verifying its integrity using CRC16 checksums.
/// The configuration is expected to be located at the given base address and have a specific layout:
/// - First 0x100 bytes: [u32 SSID length][SSID bytes...]
/// - Next 0x100 bytes: [u32 Password length][Password bytes...]
/// - Next 0x100 bytes: [u32 Mode: 0 = AP, 1 = WPA2 Personal]
/// - Next 0x100 bytes: [u32 Ident length][Ident bytes...]
/// - Last 16 bytes: [u64 CRC16 of first 0x1000-16 bytes][8 bytes magic "CHICKADE"]
///
/// # Arguments
/// * `base` - The base address in flash memory where the configuration is located.
///
/// # Returns
/// * `Some(DeviceConfig)` - The parsed device configuration if the CRC check passes and the data is valid.
/// * `None` - If the CRC check fails, the magic bytes are incorrect, or the data is otherwise invalid.
pub fn load_device_config(base: usize) -> Result<DeviceConfig, &'static str> {
    // 1. Build slice & check magic bytes
    let wifi_creds = unsafe { core::slice::from_raw_parts(base as *const u8, BLOCK_SIZE) };
    match DeviceConfig::from_bytes(wifi_creds) {
        Ok(config) => Ok(config),
        Err(e) => {
            warn!("Failed to parse WiFi credentials from flash: {}", e);
            Err(e)
        }
    }
}

impl DeviceConfig {
    /// Serialize the device configuration into a byte array suitable for writing to flash memory.
    /// The layout of the byte array is as follows:
    /// - First 0x100 bytes: [u32 SSID length][SSID bytes...]
    /// - Next 0x100 bytes: [u32 Password length][Password bytes...]
    /// - Next 0x100 bytes: [u32 Mode: 0 = AP, 1 = WPA2 Personal]
    /// - Next 0x100 bytes: [u32 Ident length][Ident bytes...]
    /// - Last 16 bytes: [u64 CRC16 of first 0x1000-16 bytes][8 bytes magic "CHICKADE"]
    ///
    /// This function always succeeds in creating the byte array, since the input data
    /// is constrained by the types (e.g., String<32> ensures the SSID and ident are not too long).
    pub fn to_bytes(&self) -> [u8; BLOCK_SIZE] {
        let mut wifi_creds = [0xffu8; BLOCK_SIZE];
        let (ssid, password) = match &self.wifi {
            WifiConf::Ap(s, p) => (s, p),
            WifiConf::Client(s, p) => (s, p),
        };
        // First block: [u32 len][SSID bytes...], total 0x100 bytes
        let ssid_bytes = ssid.as_bytes();
        wifi_creds[0..4].copy_from_slice(&(ssid_bytes.len() as u32).to_le_bytes());
        wifi_creds[4..4 + ssid_bytes.len()].copy_from_slice(ssid_bytes);
        // Second block: [u32 len][password bytes...], total 0x100 bytes
        if let Some(password) = password {
            let password_bytes = password.as_bytes();
            wifi_creds[0x100..0x104].copy_from_slice(&(password_bytes.len() as u32).to_le_bytes());
            wifi_creds[0x104..0x104 + password_bytes.len()].copy_from_slice(password_bytes);
        } else {
            wifi_creds[0x100..0x104].copy_from_slice(&0u32.to_le_bytes());
        }
        // Third block: [u32 Mode: 0 = AP, 1 = WPA2 Personal], total 0x100 bytes
        let mode_num = self.wifi.as_u32() ^ 0xffff_ffff; // Avoid all-zero bit patterns in flash
        wifi_creds[0x200..0x204].copy_from_slice(&mode_num.to_le_bytes());
        // Fourth block: [u32 len][ident bytes...], total 0x100 bytes
        let ident_bytes = self.ident.as_bytes();
        wifi_creds[0x300..0x304].copy_from_slice(&(ident_bytes.len() as u32).to_le_bytes());
        wifi_creds[0x304..0x304 + ident_bytes.len()].copy_from_slice(ident_bytes);
        // CRC bytes at the end of the page
        let crc = CrcState::calculate(&wifi_creds[0..BLOCK_SIZE - 16]) as u64;
        wifi_creds[BLOCK_SIZE - 16..BLOCK_SIZE - 8].copy_from_slice(&crc.to_le_bytes());
        // Magic bytes at the end of the page
        wifi_creds[BLOCK_SIZE - 8..BLOCK_SIZE].copy_from_slice(b"CHICKADE");
        // Return the constructed array
        wifi_creds
    }

    /// Deserialize a byte array from flash memory into a DeviceConfig struct, validating the format and CRC16 checksum.
    /// The expected layout of the byte array is as follows:
    /// - First 0x100 bytes: [u32 SSID length][SSID bytes...]
    /// - Next 0x100 bytes: [u32 Password length][Password bytes...]
    /// - Next 0x100 bytes: [u32 Mode: 0 = AP, 1 = WPA2 Personal]
    /// - Next 0x100 bytes: [u32 Ident length][Ident bytes...]
    /// - Last 16 bytes: [u64 CRC16 of first 0x1000-16 bytes][8 bytes magic "CHICKADE"]
    ///
    /// The function performs the following validations:
    /// 1. Checks that the length of the input byte array is exactly PAGE_SIZE (0x1000 bytes).
    /// 2. Verifies that the last 8 bytes of the array match the magic string "CHICKADE".
    /// 3. Calculates the CRC16 checksum of the first PAGE_SIZE - 16 bytes and compares it
    ///    against the expected CRC16 value stored in the 8 bytes preceding the magic string.
    ///
    /// # Arguments
    /// * `bytes` - A byte slice containing the raw data read from flash memory.
    ///
    /// # Returns:
    /// * `Ok(DeviceConfig)` - If the byte array is valid and the CRC check passes, returns the parsed DeviceConfig struct.
    /// * `Err(&'static str)` - If any validation fails (invalid length, magic bytes, CRC mismatch, or data parsing errors), returns an error message describing the failure.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, &'static str> {
        // 1a. Validate length
        if bytes.len() != BLOCK_SIZE {
            warn!("Invalid WiFi credentials length: {}", bytes.len());
            return Err("Invalid WiFi credentials length");
        }
        // 1b. Validate magic bytes
        if &bytes[BLOCK_SIZE - 8..BLOCK_SIZE] != b"CHICKADE" {
            warn!("Invalid WiFi credentials magic bytes");
            return Err("Invalid WiFi credentials magic bytes");
        }
        // 1c. Validate CRC16
        let expected_crc = u64::from_le_bytes(
            bytes[BLOCK_SIZE - 16..BLOCK_SIZE - 8]
                .try_into()
                .map_err(|_| "Invalid CRC bytes")?,
        ) as u16;
        let calc_crc = CrcState::calculate(&bytes[0..BLOCK_SIZE - 16]);
        if calc_crc != expected_crc {
            warn!(
                "WiFi credentials CRC mismatch: expected {:04x}, calculated {:04x}",
                expected_crc, calc_crc
            );
            return Err("WiFi credentials CRC mismatch");
        }
        // 2. Get SSID and password lengths
        let ssid_len =
            u32::from_le_bytes(bytes[0..4].try_into().map_err(|_| "Invalid SSID length")?) as usize;
        let mut password_len = u32::from_le_bytes(
            bytes[0x100..0x104]
                .try_into()
                .map_err(|_| "Invalid password length")?,
        ) as usize;
        if password_len == 0xffffffff {
            // Treat 0xffffffff as no password
            password_len = 0;
        }
        if ssid_len == 0 || ssid_len > 32 || password_len > 64 {
            warn!(
                "Invalid SSID or password length: SSID {}, Password {}",
                ssid_len, password_len
            );
            return Err("Invalid SSID or password length");
        }
        // 3. Parse SSID
        let ssid_bytes = &bytes[4..4 + ssid_len];
        let ssid = core::str::from_utf8(ssid_bytes).map_err(|_| "Invalid SSID: not valid UTF-8")?;
        // 4. Parse password
        let password = if password_len > 0 {
            Some(
                core::str::from_utf8(&bytes[0x104..0x104 + password_len])
                    .map_err(|_| "Invalid password: not valid UTF-8")?,
            )
        } else {
            None
        };
        // 5. Parse mode
        let mode = u32::from_le_bytes(
            bytes[0x200..0x204]
                .try_into()
                .map_err(|_| "Invalid WiFi mode")?,
        ) ^ 0xffff_ffff; // Avoid all-zero bit patterns in flash
        // 6. Parse
        let wifi = WifiConf::try_from((mode, ssid, password)).map_err(|_| "Invalid WiFi mode")?;
        // 7. Parse ident
        let ident_len = u32::from_le_bytes(
            bytes[0x300..0x304]
                .try_into()
                .map_err(|_| "Invalid ident length")?,
        ) as usize;
        if ident_len > 32 {
            warn!("Invalid ident length: {}", ident_len);
            return Err("Invalid ident length");
        }
        let ident_bytes = &bytes[0x304..0x304 + ident_len];
        let ident =
            core::str::from_utf8(ident_bytes).map_err(|_| "Invalid ident: not valid UTF-8")?;
        Ok(DeviceConfig {
            wifi,
            ident: String::from_str(ident).map_err(|_| "Ident string too long")?,
        })
    }
}

/// Load a firmware blob from flash memory, verifying its CRC16 checksum.
/// The blob is expected to be located at the given base address and have the specified size.
/// The provided CRC16 value is used to verify the integrity of the blob.
///
/// # Arguments
/// * `base` - The base address in flash memory where the blob is located.
/// * `size` - The size of the blob in bytes.
/// * `crc` - The expected CRC16 checksum of the blob.
///
/// # Returns
/// * `Ok(&'static Aligned<A4, [u8]>)` - A reference to the blob data if the CRC check passes.
/// * `Err(u16)` - The calculated CRC16 checksum if the check fails, indicating a mismatch with the expected value.
///
/// # Safety
/// This function is unsafe not only because it dereferences raw pointers, but also because it assumes that the provided
/// base address is 4-bytes aligned. The size may not be 4-bytes aligned (has not caused an issue), but the it is not clear
/// if it may be an issue down the line.
pub unsafe fn load_firmware_blob(
    base: usize,
    size: usize,
    crc: u16,
) -> Result<&'static Aligned<A4, [u8]>, u16> {
    let blob = unsafe { core::slice::from_raw_parts(base as *const u8, size) };
    let calc_crc = CrcState::calculate(blob);
    if calc_crc == crc {
        Ok(unsafe { &*(blob as *const [u8] as *const Aligned<A4, [u8]>) })
    } else {
        Err(calc_crc)
    }
}

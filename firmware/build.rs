//! This build script copies the `memory.x` file from the crate root into
//! a directory where the linker can always find it at build time.
//! For many projects this is optional, as the linker always searches the
//! project root directory -- wherever `Cargo.toml` is. However, if you
//! are using a workspace or have a more complicated build setup, this
//! build script becomes required. Additionally, by requesting that
//! Cargo re-run the build script whenever `memory.x` is changed,
//! updating `memory.x` ensures a rebuild of the application with the
//! new memory settings.

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::{env, str::FromStr as _};

use device_blob::{BLOCK_SIZE, DeviceConfig, WifiConf};

type CrcState = crc16::State<crc16::XMODEM>;

macro_rules! warn {
    ($($tokens: tt)*) => {
        println!("cargo::warning=WARN: {}", format!($($tokens)*))
    }
}

const KIWI_DEVICE_ID_ENV: &str = "KIWI_DEVICE_ID";
const WIFI_SSID_ENV: &str = "KIWI_WIFI_SSID";
const WIFI_PASSWORD_ENV: &str = "KIWI_WIFI_PASSWORD";
const WIFI_STARTUP_WPA2_ENV: &str = "KIWI_WIFI_WPA2";

fn main() {
    // Wifi SSID and password
    let wifi_ssid = env::var(WIFI_SSID_ENV)
        .map(|v| heapless::String::from_str(v.trim()).unwrap())
        .unwrap_or_else(|_| {
            warn!("{} not set, using default", WIFI_SSID_ENV);
            heapless::String::from_str("kiwi-ap").unwrap()
        });
    let wifi_password = env::var(WIFI_PASSWORD_ENV)
        .map(|v| heapless::String::from_str(v.trim()).unwrap())
        .inspect_err(|_| {
            warn!(
                "{} not set, using default (open network)",
                WIFI_PASSWORD_ENV
            );
        })
        .ok();
    let wifi_conf = match env::var(WIFI_STARTUP_WPA2_ENV) {
        Ok(_) => {
            warn!("{} set, using WPA2 Personal mode", WIFI_STARTUP_WPA2_ENV);
            WifiConf::Client(wifi_ssid, wifi_password)
        }
        Err(_) => {
            warn!("{} not set, using Access Point mode", WIFI_STARTUP_WPA2_ENV);
            WifiConf::Ap(wifi_ssid, wifi_password)
        }
    };
    let device_id = env::var(KIWI_DEVICE_ID_ENV)
        .map(|v| heapless::String::from_str(v.trim()).unwrap())
        .unwrap_or_else(|_| {
            warn!("{} not set, using default", KIWI_DEVICE_ID_ENV);
            heapless::String::from_str("kiwi#0001").unwrap()
        });
    let device_conf = DeviceConfig {
        ident: device_id,
        wifi: wifi_conf,
    };
    // Paths
    let firmware_path = PathBuf::from("../cyw43-firmware/43439A0.bin");
    let clm_path = PathBuf::from("../cyw43-firmware/43439A0_clm.bin");
    let memx_path = PathBuf::from("../config/memory.x.template");
    // Manifest directory, i.e. the directory containing Cargo.toml
    let manifest_dir_str = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let curdir = PathBuf::from(manifest_dir_str);
    // Calculate WiFi firmware and CLM CRCs and lengths
    let wifi_fw = std::fs::read(&firmware_path).expect("Failed to read WiFi firmware");
    let wifi_fw_len = wifi_fw.len();
    let wifi_fw_crc = CrcState::calculate(&wifi_fw);
    let wifi_clm = std::fs::read(&clm_path).expect("Failed to read WiFi CLM");
    let wifi_clm_len = wifi_clm.len();
    let wifi_clm_crc = CrcState::calculate(&wifi_clm);

    // Read the template memory.x file
    let memx = std::fs::read_to_string(&memx_path).expect("Failed to read memory.x.template");

    // Regex to extract flash size and base address
    let flash_re = regex::Regex::new(r"LENGTH = (\d+)K").unwrap();
    let base_re = regex::Regex::new(r"FLASH : ORIGIN = 0x([a-fA-F0-9]+)").unwrap();
    // Flash size

    let flash_len_k = flash_re.captures(&memx).unwrap().get(1).unwrap().as_str(); // in KiB
    let flash_len: usize = flash_len_k.parse::<usize>().unwrap() * 1024; // in bytes

    // Base address
    let base_addr = base_re.captures(&memx).unwrap().get(1).unwrap().as_str();
    let mem_base = usize::from_str_radix(base_addr, 16).unwrap();
    // Calculate WiFi firmware and CLM addresses
    let wifi_clm_ofst = (flash_len - wifi_clm_len) & !0xfff; // align to 4K
    let wifi_clm_addr = wifi_clm_ofst + mem_base;
    let wifi_fw_ofst: usize = (wifi_clm_ofst - wifi_fw_len) & !0xfff; // align to 4K
    let wifi_fw_addr = wifi_fw_ofst + mem_base;
    // Calculate WiFi SSID and password addresses
    let wifi_creds_ofst = wifi_fw_ofst - BLOCK_SIZE; // 4KB page before WiFi firmware
    let wifi_creds_addr = wifi_creds_ofst + mem_base;
    if wifi_creds_ofst < BLOCK_SIZE {
        panic!(
            "Not enough flash memory for WiFi firmware and credentials! Larger flash size required."
        );
    }
    // Create WiFi credentials blob
    let wifi_creds = device_conf.to_bytes();
    // Calculate WiFi credentials CRC
    let wifi_creds_crc = CrcState::calculate(&wifi_creds);
    // See if the credentials exist, and if they do, check if they match
    if let Ok(existing_creds) = std::fs::read(curdir.join("wifi_creds.bin")) {
        let existing_crc = CrcState::calculate(&existing_creds);
        if existing_crc != wifi_creds_crc || existing_creds != wifi_creds {
            warn!(
                "WiFi credentials file already exists but do not match the provided WIFI_SSID and WIFI_PASSWORD environment variables. Overwriting."
            );
            warn!("Re-flash the WiFi credentials using the generated flash-wifi-fw script.");
        }
    }
    // Write WiFi credentials blob to flash
    File::create(curdir.join("wifi_creds.bin"))
        .unwrap()
        .write_all(&wifi_creds)
        .unwrap();

    // Calculate free memory before the WiFi firmware blobs
    let free_mem = (wifi_fw_ofst - BLOCK_SIZE) & !(BLOCK_SIZE - 1); // align to 4K
    if free_mem + mem_base > wifi_creds_addr {
        panic!(
            "Not enough free flash memory before WiFi firmware and credentials! Larger flash size required."
        );
    }

    // Write to src/wifi_fw_consts.rs
    File::create(curdir.join("src").join("wifi_fw_consts.rs"))
        .unwrap()
        .write_all(
            format!(
                "\
#![allow(dead_code)]
/// Base address where the WiFi firmware blob is stored in flash
pub const WIFI_FW_ADDR: usize = 0x{wifi_fw_addr:x};
/// Length of the WiFi firmware blob in bytes
pub const WIFI_FW_LEN: usize = {wifi_fw_len};
/// CRC16/XMODEM of the WiFi firmware blob
pub const WIFI_FW_CRC: u16 = 0x{wifi_fw_crc:04X};
/// Base address where the WiFi Control Mode blob is stored in flash
pub const WIFI_CLM_ADDR: usize = 0x{wifi_clm_addr:x};
/// Length of the WiFi Control Mode blob in bytes
pub const WIFI_CLM_LEN: usize = {wifi_clm_len};
/// CRC16/XMODEM of the WiFi Control Mode blob
pub const WIFI_CLM_CRC: u16 = 0x{wifi_clm_crc:04X};

/// Base address where the WiFi credentials blob is stored in flash
pub const WIFI_CREDS_ADDR: usize = 0x{wifi_creds_addr:x};
/// WiFi credentials offset in flash
pub const WIFI_CREDS_OFST: u32 = 0x{wifi_creds_ofst:x};

/// Flash memory size in bytes, up to the start of WiFi firmware.
/// This ensures code inside the firmware can not overwrite
/// the WiFi firmware, but allows writing the WiFi credentials.
pub const FLASH_SIZE: usize = 0x{wifi_fw_ofst:x};

/// Free flash memory before WiFi firmware and credentials in bytes
pub const FREE_FLASH_SIZE: usize = 0x{free_mem:x};
",
            )
            .as_bytes(),
        )
        .unwrap();

    // Replace the LENGTH field in memory.x
    let free_mem = free_mem / 1024; // in KiB
    let memx = flash_re.replace(&memx, format!("LENGTH = {free_mem}K"));

    // Put `memory.x` in our output directory and ensure it's
    // on the linker search path.
    let outdir = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
    File::create(outdir.join("memory.x"))
        .unwrap()
        .write_all(memx.as_bytes())
        .unwrap();

    // Build the WiFi firmware flasher
    #[cfg(target_family = "unix")]
    {
        use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
        let flasher = curdir.join("flash-wifi-fw.sh");
        if !flasher.exists() {
            warn!(
                "Creating flash-wifi-fw.sh script to flash WiFi firmware, CLM, and credentials. Run this script BEFORE flashing the main firmware."
            );
        }
        let flash_cmd = format!(
            "#!/bin/sh\n\
            set -e\n\
            probe-rs download {} --binary-format bin --chip RP235x --base-address 0x{wifi_fw_addr:x}\n\
            probe-rs download {} --binary-format bin --chip RP235x --base-address 0x{wifi_clm_addr:x}\n\
            probe-rs download wifi_creds.bin --binary-format bin --chip RP235x --base-address 0x{wifi_creds_addr:x}\n",
            firmware_path.display(),
            clm_path.display(),
        );
        OpenOptions::new()
            .mode(0o754)
            .create(true)
            .truncate(true)
            .write(true)
            .open(flasher)
            .unwrap()
            .write_all(flash_cmd.as_bytes())
            .unwrap();
    }
    // For Windows, just create a .bat file (not tested)
    #[cfg(target_family = "windows")]
    {
        let flasher = curdir.join("flash-wifi-fw.bat");
        if !flasher.exists() {
            warn!(
                "Creating flash-wifi-fw.bat script to flash WiFi firmware, CLM, and credentials. Run this script BEFORE flashing the main firmware."
            );
        }
        let flash_cmd = format!(
            "@echo off\r\n\
            setlocal enabledelayedexpansion\r\n\
            probe-rs download {} --binary-format bin --chip RP235x --base-address 0x{wifi_fw_addr:x}\r\n\
            probe-rs download {} --binary-format bin --chip RP235x --base-address 0x{wifi_clm_addr:x}\r\n\
            probe-rs download wifi_creds.bin --binary-format bin --chip RP235x --base-address 0x{wifi_creds_addr:x}\r\n",
            firmware_path.display(),
            clm_path.display(),
        );
        File::create(flasher)
            .unwrap()
            .write_all(flash_cmd.as_bytes())
            .unwrap();
    }
    // Tell cargo to tell rustc to link the memory.x file
    println!("cargo:rustc-link-search={}", outdir.display());

    // By default, Cargo will re-run a build script whenever
    // any file in the project changes. By specifying `memory.x`
    // here, we ensure the build script is only re-run when
    // `memory.x` is changed.
    println!("cargo:rerun-if-changed=../../config/memory.x.template");
    println!("cargo:rerun-if-changed={}", firmware_path.display());
    println!("cargo:rerun-if-changed={}", clm_path.display());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=flash-wifi-fw.sh");
    println!("cargo:rerun-if-changed=flash-wifi-fw.bat");
}

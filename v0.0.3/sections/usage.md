<section class="manual-sheet" id="usage" markdown="1">

# Using the Kiwi

Once flashed, Kiwi streams every sensor measurement over one radio at a time, Wi-Fi or Bluetooth Low-Energy (BLE), and logs it to a microSD card if one is inserted. It is configured over its USB serial interface.

## Radio Modes

Kiwi boots in **Wi-Fi mode** (or in **BLE mode** when built with the `ble-default` feature). Press the `BOOTSEL` button at any time to switch between the two. Each press toggles the mode, including while Kiwi is still trying to join a Wi-Fi network.

### Wi-Fi

Depending on its configuration, Kiwi either hosts its own network or joins a Wi-Fi network:

| Configuration | Behaviour |
|---------------|-----------|
| **Access point** (`wifi ap`) | Kiwi hosts a Wi-Fi network on channel 5 (open, or WPA2 with a password). Kiwi's address on it is `169.254.1.1`. |
| **Client** (`wifi cl`) | Kiwi joins the configured network and gets an address by DHCP. |

If Kiwi cannot join the configured network within 60 seconds, it falls back to hosting an **open access point named after its device identifier**, so it stays reachable.

In Wi-Fi mode, measurements are broadcast as UDP datagrams to port **8099**, one measurement per datagram (see [Data Format](#data-format)).
In client mode, Kiwi also sends its Wi-Fi signal strength (RSSI) every 2 seconds.

### Bluetooth Low Energy (BLE)

In BLE mode, Kiwi advertises under its device identifier. Up to **four** clients can connect at the same time without pairing.
Each client receives every measurement as a notification, along with the signal strength (RSSI) of the BLE link every 2 seconds.

| Item | UUID |
|------|------|
| Kiwi data service | `6b697769-0001-4a6e-8f3b-2c9d5e7a1b00` |
| Measurement characteristic (read, notify) | `6b697769-0002-4a6e-8f3b-2c9d5e7a1b00` |

Subscribe to notifications on the measurement characteristic.
Each notification carries one or more consecutive measurements (up to five) within the negotiated ATT MTU, so clients must negotiate an MTU of at least 27 bytes.
A client that cannot keep up loses its oldest measurements; other clients are not affected.

## Data Format

Every measurement, whether sent over UDP, BLE, or written to the microSD card, is a fixed-size **24-byte** `SingleMeasurement` record from the [`kiwi-measurements`](https://crates.io/crates/kiwi-measurements) crate, which can decode it. Besides sensor readings, Kiwi sends its device identifier every 15 seconds and signal-strength (RSSI) records, so clients can name the device and show link quality.

## microSD Logging

If a microSD card is inserted, Kiwi records every measurement to it. Each power-on starts a new run directory:

```text
KIWI/
  README.TXT      describes this layout
  00001/          firmware run 1
    00000.BIN     first data segment
    00001.BIN     next segment (started after 3 GiB)
  00002/          firmware run 2 (next power-on)
```

Each `.BIN` file starts with a 256-byte text header, followed by 24-byte measurement records. To parse a file, skip the first 256 bytes, then read every 24 bytes as one record.

## Serial Commands

Kiwi appears on your computer as a USB device named **Kiwi Mainboard Rev. B** with two serial ports. The **first** is the command console; the second carries the firmware's binary log stream (read by `probe-rp-usb`). Open the command console with any serial terminal, for example:

| Platform | Port | Example |
|----------|------|---------|
| Linux | `/dev/ttyACM0` (first of the pair) | `screen /dev/ttyACM0` |
| macOS | `/dev/cu.usbmodem…` (first of the pair) | `screen /dev/cu.usbmodem…` |
| Windows | `COMn` (first of the pair) | PuTTY, or the VS Code Serial Monitor |

Kiwi shows a `>` prompt. Type a command and press <kbd>Enter</kbd>:

| Command | Effect |
|---------|--------|
| `help` | List the available commands |
| `version` | Show the firmware version |
| `wifi status` | Show the stored configuration and the current data rate |
| `ident get` | Show the device identifier |
| `ident set <ident>` | Set the device identifier (up to 12 characters) |
| `wifi ap <ssid> [password]` | Host an access point named `<ssid>`, open if no password is given |
| `wifi cl <ssid> [password]` | Join the network `<ssid>` as a client, open if no password is given |
| `show` | Show the changes made but not yet stored |
| `store` | Save the changes to flash and restart Kiwi |
| `reset` | Restart Kiwi |
| `clear` | Clear the terminal screen |

Passwords must be at least 8 characters. `ident set`, `wifi ap` and `wifi cl` only take effect after `store`, which saves them and restarts Kiwi. For example, to join a network called `lab-net`:

```text
> wifi cl lab-net mypassword
> store
```

## Client Software

[Kiwi Plotter](https://github.com/chickadee-smallsat/kiwi-mainboard-clients/releases/latest) is a desktop app for Linux, Windows, and macOS that receives data from Kiwi devices over both Wi-Fi and BLE, plots it live, and exports it to CSV or Excel files.
To write your own client, listen for UDP datagrams on port 8099 or subscribe to the BLE characteristic above, and decode the records with `kiwi-measurements`.

</section>

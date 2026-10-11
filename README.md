# rpiboot-gui

A small macOS desktop app around Raspberry Pi's [usbboot](https://github.com/raspberrypi/usbboot)
(`rpiboot`) for Compute Modules with eMMC (CM3, CM4, CM5):

- **Boot order** — reorder and enable the boot sources with buttons instead of
  working out the `BOOT_ORDER` hex by hand. The resulting value is shown live.
- **Flash** — writes the boot order into usbboot's `recovery/boot.conf`,
  regenerates `pieeprom.bin` with usbboot's own `update-pieeprom.sh`, and runs
  `rpiboot -d recovery` so the module flashes its EEPROM.
- **Mount** — runs `rpiboot -d mass-storage-gadget64`, then waits for the eMMC
  to show up as a disk on the Mac and prints its `diskutil` listing.

Everything the app does is the documented usbboot procedure; it just drives it.
The log pane shows every command and its output.

## Requirements

- macOS, with the Xcode command line tools, Homebrew `libusb` and `pkg-config`
  (to build `rpiboot`), and `python3` (used by usbboot's `rpi-eeprom-config`).
- A clone of usbboot. The app defaults to `~/Desktop/usbboot`; point it at
  yours in the top field. The **Prepare usbboot** button runs `make` and
  fetches the `rpi-eeprom` submodule (the bootloader images) if they are
  missing.
- Rust (`cargo`) to build this app.

## Run

```
cargo run --release
```

## Use

1. Fit the module's `nRPIBOOT` / "disable eMMC boot" jumper on the carrier
   (CM4 IO board J2, or the equivalent switch), connect the carrier's USB
   device port to the Mac with a data cable, then power the carrier.
2. **Flash boot order**: arrange the sources, pick what happens at the end of
   the list (restart the list or stop), press the button. The log ends with
   `"EEPROM_UPDATE": "success"` when the module has written its EEPROM. The
   module's activity LED blinks steadily. Power-cycle it afterwards.
3. **Mount eMMC**: with the jumper still fitted and the module freshly
   powered, press the button. After a few seconds the eMMC appears as
   `/dev/diskN`; its FAT partition is auto-mounted under `/Volumes`.
   Write an image to it with `diskutil unmountDisk /dev/diskN` followed by
   `sudo dd if=image.img of=/dev/rdiskN bs=4m`.

`rpiboot` does not need root on macOS; `dd` to the raw disk does, so the app
leaves that step to you.

## Boot order codes

| Code | Source |
|---|---|
| 1 | SD card / eMMC |
| 2 | Network (TFTP) |
| 3 | RPIBOOT (USB device mode) |
| 4 | USB mass storage |
| 5 | BCM USB mass storage (CM4 USB-C) |
| 6 | NVMe (M.2 / PCIe) |
| 7 | HTTP |
| e | Stop |
| f | Restart the list |

The firmware reads `BOOT_ORDER` from the right: `0xf2461` is eMMC, then NVMe,
then USB mass storage, then network, then start over.

## License

MIT.

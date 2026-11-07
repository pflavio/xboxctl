---
permalink: /nano/
title: Xbox Nano (experimental)
nav_order: 5
description: "Experimental Xbox Nano detection and descriptor capture on Linux: current limits, evidence and the work needed for firmware updates."
---

# Xbox Nano support (experimental)
{: .no_toc }

This branch recognizes the Xbox Nano Controller's USB ID `045e:0b4e` and
supports inspection through `list`, `info` and `probe`. **Nano firmware
flashing is not implemented.** Its HID update protocol and full firmware
version have not been established.

| Command | Nano behavior |
|---------|---------------|
| `list` | Identifies the Nano and marks its HID transport as inspection only |
| `info` | Shows USB identity and device revision, without opening a GIP session |
| `probe` | Captures USB and cached HID descriptors as JSON |
| GIP operations, including firmware updates | Refused for the Nano HID transport |

The Nano's observed `bcdDevice` value is `0x0609` (`6.09` in `lsusb`). This is
the **USB device revision**, not a confirmed full firmware version or evidence
that the firmware is current.

## Why adding the USB ID is insufficient

The initial Linux capture exposes one active USB interface:

| Property | Observed Nano | Existing xboxctl GIP transport |
|----------|---------------|-------------------------------|
| Interface class/subclass/protocol | HID `03/00/00` | `ff/47/d0` |
| Interrupt IN endpoint | `0x81` | `0x82` |
| Interrupt OUT endpoint | `0x02` | `0x02` |
| HID report descriptor | Advertised length 285 bytes | Not used |

[Released xboxctl 0.8.1](https://github.com/bb441db/xboxctl/blob/0.8.1/src/usb.rs)
discovers GIP interfaces. A friendly-name entry for the Nano cannot make its
HID interface speak GIP, and changing the endpoint address cannot establish
the report framing or firmware commands. An independent
[SDL hardware report](https://github.com/libsdl-org/SDL/issues/16657) also
identifies the Nano as `045e:0b4e`, with USB revision `0x0609`.

The capture's device qualifier contains `ff/47/d0`, but those device-level
fields describe other-speed properties, not the active interface. Microsoft's
[GIP USB specification](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-gipusb/86149e9c-ff03-4eb7-a10b-c074c22a7e01)
requires GIP devices to reject the qualifier request because that transport is
Full Speed (12 Mbps). The Nano's qualifier does not establish a usable GIP
mode or a faster-port workaround.

## Firmware feed findings

Both official Microsoft GA feeds were fetched on **2026-11-07**. Neither
contained an entry for the exact USB identity `045E_0B4E`.

| Feed | Advertised version | Index observed on that date |
|------|--------------------|-----------------------------|
| [PC GA](https://aka.ms/xboxaccessoriesfwcdnpcga) | `pc_ga` / `1.0.0.23` | [PC index](https://xbaccessories-bhf7bsbfgxdpbrg4.b02.azurefd.net/accessories/M/PC_GA_hqZ1pdWMjUW9sduWoLPDUQ.json) |
| [Xbox GA](https://aka.ms/xboxaccessoriesfwcdnxbga) | `xbox_ga` / `1.0.0.20` | [Xbox index](https://xbaccessories-bhf7bsbfgxdpbrg4.b02.azurefd.net/accessories/M/XB_GA_ew92RW1KHEGfgr6ZoY3DyQ.json) |

The `nico` entry in both feeds has identity `045E_0B32-cc-BBM`. It is not a
verified mapping for the Nano and must not be selected from the similarity
of its name. These findings concern those dated feeds; they do not rule out
a separate Microsoft distribution channel or a later feed change.

## Capture a Nano and a Series controller on Linux

Connect the Nano and a regular Xbox Series X|S controller with USB data
cables. From this branch's checkout, with Rust and Cargo installed, run:

```bash
cargo build --release --locked
./target/release/xboxctl list
./target/release/xboxctl probe --all --output nano-and-series.json
```

`list` confirms detection. `probe --all` captures all detected controllers,
so the Series controller provides a GIP comparison in the same report.
The probe reads descriptors without claiming an interface, detaching a
driver, or sending controller protocol commands. It does not request HID
feature reports or change controller state.

`--output` creates a **new file** and refuses to overwrite an existing one.
Choose an unused filename for each capture. A partial report can still be
useful when a descriptor is unavailable; inspect its recorded errors.
Only if the capture reports permission denied, retry the probe with `sudo`
and another unused filename:

```bash
sudo ./target/release/xboxctl probe --all --output nano-and-series-sudo.json
```

Share the JSON capture, which omits serial numbers. If including `list`
output, remove any serial numbers from it first.

On Linux, `kernel_report_descriptor` comes from the kernel's cached HID
descriptor. A driver may have applied fixups, so this is not necessarily a
byte-for-byte USB capture. The report includes the bound driver names to
preserve that context. See the kernel's
[HID descriptor guide](https://github.com/torvalds/linux/blob/master/Documentation/hid/hidintro.rst)
and [sysfs implementation](https://github.com/torvalds/linux/blob/master/drivers/hid/hid-core.c).

## Next implementation work

The first step is to inspect the report descriptor's collections, usages,
report IDs and lengths. Those describe the HID layout; they do not by
themselves define vendor firmware commands.

Next, correlate any vendor collections with official driver metadata and
firmware package identities. If necessary, a contributor's trace of the
official software can establish the version-query and update exchanges.
No confirmed Nano HID update protocol was found in the primary sources
reviewed on 2026-11-07.

Firmware support requires evidence for the transport, package matching,
transfer acknowledgements, restart behavior and recovery. Keep Nano protocol
writes disabled until that evidence supports an implementation, and use the
Series controller to check that the existing GIP path continues to work.

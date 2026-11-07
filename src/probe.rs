//! Read-only USB descriptors for investigating a controller's transport.
//!
//! This deliberately never opens a USB handle, claims an interface, or sends
//! controller commands. On Linux the HID report descriptor is read from the
//! kernel's sysfs cache. USB strings, HID unique identifiers and input reports
//! are not read, and no data is uploaded.

use crate::usb::{self, Controller};
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    io::{self, Write},
    path::Path,
};

/// Capture one selected controller, or all recognized controllers together.
/// Without `output`, stdout contains JSON only. Existing files are not replaced.
pub fn run(device: Option<&str>, all: bool, output: Option<&Path>) -> Result<(), String> {
    if all && device.is_some() {
        return Err("choose either --all or --device, not both".to_owned());
    }
    let controllers = if all {
        usb::find_controllers()?
    } else {
        vec![usb::select_controller(device)?]
    };
    if controllers.is_empty() {
        return Err("no Xbox controller found".to_owned());
    }
    let report = json!({
        "schema_version": 1,
        "xboxctl_version": env!("CARGO_PKG_VERSION"),
        "platform": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        },
        "capture": {
            "read_only": true,
            "usb_strings_read": false,
            "hid_unique_identifiers_read": false,
            "input_reports_read": false,
        },
        "controllers": controllers.iter().map(capture_controller).collect::<Vec<_>>(),
    });
    let mut bytes = serde_json::to_vec_pretty(&report)
        .map_err(|error| format!("could not encode descriptor report: {error}"))?;
    bytes.push(b'\n');
    if let Some(path) = output {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| format!("could not create {}: {error}", path.display()))?;
        file.write_all(&bytes)
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;
        eprintln!("Saved USB descriptor report to {}", path.display());
    } else {
        io::stdout()
            .lock()
            .write_all(&bytes)
            .map_err(|error| format!("could not write descriptor report: {error}"))?;
    }
    Ok(())
}

fn capture_controller(controller: &Controller) -> Value {
    let mut report = json!({
        "location": controller.location(),
        "vendor_id": format!("0x{:04x}", controller.vendor_id),
        "product_id": format!("0x{:04x}", controller.product_id),
        "model": controller.model().0,
        "transport": match controller.transport {
            usb::Transport::Gip => "gip",
            usb::Transport::NanoHid => "nano-hid-inspection-only",
        },
        // bcdDevice is a device-defined revision, not a complete firmware version.
        "bcd_device": format!("0x{:04x}", controller.bcd_device),
        "speed": format!("{:?}", controller.device.speed()),
    });
    let descriptor = match controller.device.device_descriptor() {
        Ok(descriptor) => descriptor,
        Err(error) => {
            report["device_descriptor_error"] = json!(error.to_string());
            return report;
        }
    };
    let version = descriptor.usb_version();
    let bcd_usb = u16::from(version.major() / 10) << 12
        | u16::from(version.major() % 10) << 8
        | u16::from(version.minor()) << 4
        | u16::from(version.sub_minor());
    report["device_descriptor"] = json!({
        "bcd_usb": format!("0x{bcd_usb:04x}"),
        "class": format!("0x{:02x}", descriptor.class_code()),
        "subclass": format!("0x{:02x}", descriptor.sub_class_code()),
        "protocol": format!("0x{:02x}", descriptor.protocol_code()),
        "max_packet_size_0": descriptor.max_packet_size(),
        "num_configurations": descriptor.num_configurations(),
    });
    let active = match controller.device.active_config_descriptor() {
        Ok(config) => Some(config.number()),
        Err(error) => {
            report["active_configuration_error"] = json!(error.to_string());
            None
        }
    };
    report["active_configuration"] = json!(active);
    let configurations: Vec<_> = (0..descriptor.num_configurations())
        .map(|index| match controller.device.config_descriptor(index) {
            Ok(config) => {
                let interfaces: Vec<_> = config
                    .interfaces()
                    .flat_map(|interface| interface.descriptors())
                    .map(|interface| capture_interface(controller, config.number(), &interface))
                    .collect();
                json!({
                    "index": index,
                    "number": config.number(),
                    "total_length": config.total_length(),
                    "num_interfaces": config.num_interfaces(),
                    "self_powered": config.self_powered(),
                    "remote_wakeup": config.remote_wakeup(),
                    "max_power_ma": config.max_power(),
                    "extra_descriptors_hex": usb::hex(config.extra()),
                    "interfaces": interfaces,
                })
            }
            Err(error) => json!({ "index": index, "error": error.to_string() }),
        })
        .collect();
    report["configurations"] = json!(configurations);
    report
}

fn capture_interface(
    controller: &Controller,
    configuration: u8,
    interface: &rusb::InterfaceDescriptor<'_>,
) -> Value {
    let endpoints: Vec<_> = interface
        .endpoint_descriptors()
        .map(|endpoint| {
            json!({
                "address": format!("0x{:02x}", endpoint.address()),
                "direction": format!("{:?}", endpoint.direction()),
                "transfer_type": format!("{:?}", endpoint.transfer_type()),
                "max_packet_size": endpoint.max_packet_size(),
                "interval": endpoint.interval(),
                "extra_descriptors_hex": usb::hex(endpoint.extra().unwrap_or_default()),
            })
        })
        .collect();
    let mut report = json!({
        "number": interface.interface_number(),
        "alternate_setting": interface.setting_number(),
        "class": format!("0x{:02x}", interface.class_code()),
        "subclass": format!("0x{:02x}", interface.sub_class_code()),
        "protocol": format!("0x{:02x}", interface.protocol_code()),
        "num_endpoints": interface.num_endpoints(),
        "extra_descriptors_hex": usb::hex(interface.extra()),
        "endpoints": endpoints,
    });
    if interface.class_code() == 0x03 {
        let lengths = match hid_report_lengths(interface.extra()) {
            Ok(lengths) => lengths,
            Err(error) => {
                report["hid_descriptor_error"] = json!(error);
                Vec::new()
            }
        };
        report["declared_report_descriptor_lengths"] = json!(lengths);
        let expected = (lengths.len() == 1).then(|| lengths[0]);
        #[cfg(target_os = "linux")]
        {
            report["kernel_report_descriptor"] = linux::capture(
                Path::new("/sys/bus/hid/devices"),
                linux::Key {
                    bus: controller.bus,
                    address: controller.address,
                    vendor_id: controller.vendor_id,
                    product_id: controller.product_id,
                    configuration,
                    interface: interface.interface_number(),
                    alternate_setting: interface.setting_number(),
                },
                expected,
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (controller, configuration, expected);
            report["kernel_report_descriptor"] = json!({
                "status": "unavailable",
                "reason": "Kernel-cached HID report capture is currently supported on Linux only.",
            });
        }
    }
    report
}

/// Extract report descriptor lengths from interface extras, without interpreting
/// vendor reports or assuming that a HID output report carries GIP packets.
fn hid_report_lengths(mut extra: &[u8]) -> Result<Vec<u16>, String> {
    let mut lengths = Vec::new();
    while !extra.is_empty() {
        if extra.len() < 2 || extra[0] < 2 || usize::from(extra[0]) > extra.len() {
            return Err("truncated or invalid extra descriptor".to_owned());
        }
        let (descriptor, remaining) = extra.split_at(usize::from(extra[0]));
        extra = remaining;
        if descriptor[1] != 0x21 {
            continue;
        }
        if descriptor.len() < 6 || descriptor.len() < 6 + 3 * usize::from(descriptor[5]) {
            return Err("truncated HID descriptor".to_owned());
        }
        for entry in descriptor[6..]
            .as_chunks::<3>()
            .0
            .iter()
            .take(usize::from(descriptor[5]))
        {
            if entry[0] == 0x22 {
                lengths.push(u16::from_le_bytes([entry[1], entry[2]]));
            }
        }
    }
    if lengths.is_empty() {
        return Err("no HID report descriptor length in interface extras".to_owned());
    }
    Ok(lengths)
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::{fs, io::Read, path::PathBuf};

    // The USB HID descriptor represents this length in a u16. Read one extra
    // byte to detect truncation without ever allocating an unbounded buffer.
    const MAX_REPORT_DESCRIPTOR: usize = u16::MAX as usize;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) struct Key {
        pub bus: u8,
        pub address: u8,
        pub vendor_id: u16,
        pub product_id: u16,
        pub configuration: u8,
        pub interface: u8,
        pub alternate_setting: u8,
    }

    fn read_bounded(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "descriptor exceeds size limit",
            ));
        }
        Ok(bytes)
    }

    fn number(path: &Path, name: &str, radix: u32) -> Option<u16> {
        let bytes = read_bounded(&path.join(name), 128).ok()?;
        u16::from_str_radix(std::str::from_utf8(&bytes).ok()?.trim(), radix).ok()
    }

    fn key(path: &Path) -> Option<Key> {
        // Only a HID node descended from a USB interface can match. A Bluetooth
        // HID node with the same VID/PID must not be associated with this device.
        let interface = path
            .ancestors()
            .find(|path| path.join("bInterfaceNumber").is_file())?;
        let device = interface
            .ancestors()
            .skip(1)
            .find(|path| path.join("idVendor").is_file())?;
        Some(Key {
            bus: number(device, "busnum", 10)?.try_into().ok()?,
            address: number(device, "devnum", 10)?.try_into().ok()?,
            vendor_id: number(device, "idVendor", 16)?,
            product_id: number(device, "idProduct", 16)?,
            configuration: number(device, "bConfigurationValue", 10)?.try_into().ok()?,
            interface: number(interface, "bInterfaceNumber", 16)?.try_into().ok()?,
            // Unlike bInterfaceNumber, Linux exports bAlternateSetting in decimal.
            alternate_setting: number(interface, "bAlternateSetting", 10)?
                .try_into()
                .ok()?,
        })
    }

    fn unavailable(reason: impl ToString) -> Value {
        json!({
            "status": "unavailable",
            "source": "linux-sysfs",
            "reason": reason.to_string(),
        })
    }

    fn driver_name(path: &Path) -> Option<String> {
        // Only the driver's name, never the full sysfs path, belongs in a report.
        fs::read_link(path.join("driver"))
            .ok()?
            .file_name()?
            .to_str()
            .map(str::to_owned)
    }

    pub(super) fn capture(root: &Path, expected: Key, declared_length: Option<u16>) -> Value {
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) => {
                return unavailable(format!("could not enumerate Linux HID devices: {error}"));
            }
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.path().canonicalize().ok())
            .filter(|path| key(path) == Some(expected))
            .collect();
        paths.sort();
        paths.dedup();
        let path = match paths.as_slice() {
            [path] => path,
            [] => {
                return unavailable(
                    "no kernel HID descriptor matches this USB device, configuration and interface; \
                 keep it connected and check that the HID driver is bound",
                );
            }
            _ => {
                return unavailable(
                    "multiple HID devices match this USB interface; descriptor is ambiguous",
                );
            }
        };
        let bytes = match read_bounded(&path.join("report_descriptor"), MAX_REPORT_DESCRIPTOR) {
            Ok(bytes) if !bytes.is_empty() => bytes,
            Ok(_) => return unavailable("the kernel HID report descriptor is empty"),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                return unavailable(
                    "permission denied reading the kernel HID report descriptor; run the probe with sudo",
                );
            }
            Err(error) => {
                return unavailable(format!(
                    "could not read kernel HID report descriptor: {error}"
                ));
            }
        };
        // Avoid attaching a cached descriptor to a device that disconnected or
        // changed configuration during this capture.
        if key(path) != Some(expected) {
            return unavailable(
                "USB device or interface changed during capture; run the probe again",
            );
        }
        let mismatch = declared_length.is_some_and(|length| usize::from(length) != bytes.len());
        let mut report = json!({
            "status": if mismatch { "length_mismatch" } else { "ok" },
            "source": "linux-sysfs",
            "may_include_driver_fixups": true,
            "hid_driver": driver_name(path),
            "usb_driver": path.ancestors()
                .find(|path| path.join("bInterfaceNumber").is_file())
                .and_then(driver_name),
            "length": bytes.len(),
            "hex": usb::hex(&bytes),
        });
        if mismatch {
            report["warning"] = json!(
                "kernel-cached descriptor length differs from the USB HID declaration; \
                 a driver fixup or a changed device may explain the difference"
            );
        }
        report
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::atomic::{AtomicUsize, Ordering};

        static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

        struct Fixture(PathBuf);

        impl Fixture {
            fn new() -> Self {
                let path = std::env::temp_dir().join(format!(
                    "xboxctl-probe-{}-{}",
                    std::process::id(),
                    NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
                ));
                fs::create_dir(&path).unwrap();
                fs::create_dir(path.join("hid-devices")).unwrap();
                Self(path)
            }

            fn root(&self) -> PathBuf {
                self.0.join("hid-devices")
            }

            fn add(&self, name: &str, key: Key, report: &[u8]) {
                let device = self.0.join(name);
                let interface = device.join("interface");
                let hid = interface.join("hid");
                fs::create_dir_all(&hid).unwrap();
                for (name, value) in [
                    ("busnum", key.bus.to_string()),
                    ("devnum", key.address.to_string()),
                    ("idVendor", format!("{:04x}", key.vendor_id)),
                    ("idProduct", format!("{:04x}", key.product_id)),
                    ("bConfigurationValue", key.configuration.to_string()),
                    ("serial", "PRIVATE_SERIAL_MUST_NOT_APPEAR".to_owned()),
                ] {
                    fs::write(device.join(name), value).unwrap();
                }
                fs::write(
                    interface.join("bInterfaceNumber"),
                    format!("{:02x}", key.interface),
                )
                .unwrap();
                fs::write(
                    interface.join("bAlternateSetting"),
                    key.alternate_setting.to_string(),
                )
                .unwrap();
                fs::write(hid.join("uniq"), "PRIVATE_HID_UNIQUE_MUST_NOT_APPEAR").unwrap();
                fs::write(hid.join("report_descriptor"), report).unwrap();
                std::os::unix::fs::symlink("/sys/bus/hid/drivers/hid-generic", hid.join("driver"))
                    .unwrap();
                std::os::unix::fs::symlink("/sys/bus/usb/drivers/usbhid", interface.join("driver"))
                    .unwrap();
                std::os::unix::fs::symlink(hid, self.root().join(name)).unwrap();
            }
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        fn nano() -> Key {
            Key {
                bus: 3,
                address: 14,
                vendor_id: 0x045e,
                product_id: 0x0b4e,
                configuration: 1,
                interface: 0,
                alternate_setting: 0,
            }
        }

        #[test]
        fn matches_exact_device_and_does_not_include_private_identifiers() {
            let fixture = Fixture::new();
            // These are synthetic descriptor bytes, not a captured Nano report.
            fixture.add("target", nano(), &[0x05, 0x01]);
            fixture.add(
                "same-model",
                Key {
                    address: 15,
                    ..nano()
                },
                &[0x09, 0x05],
            );
            let report = capture(&fixture.root(), nano(), Some(2));
            assert_eq!(report["status"], "ok");
            assert_eq!(report["hex"], "05 01");
            assert_eq!(report["hid_driver"], "hid-generic");
            assert_eq!(report["usb_driver"], "usbhid");
            let json = report.to_string();
            assert!(!json.contains("PRIVATE_"));
            assert!(!json.contains(fixture.0.to_str().unwrap()));
        }

        #[test]
        fn rejects_different_bus_ids_configuration_interface_and_alt_setting() {
            let fixture = Fixture::new();
            fixture.add("target", nano(), &[0x05, 0x01]);
            for wrong in [
                Key { bus: 4, ..nano() },
                Key {
                    address: 15,
                    ..nano()
                },
                Key {
                    vendor_id: 0x1234,
                    ..nano()
                },
                Key {
                    product_id: 0x0b12,
                    ..nano()
                },
                Key {
                    configuration: 2,
                    ..nano()
                },
                Key {
                    interface: 1,
                    ..nano()
                },
                Key {
                    alternate_setting: 1,
                    ..nano()
                },
            ] {
                assert_eq!(
                    capture(&fixture.root(), wrong, Some(2))["status"],
                    "unavailable"
                );
            }
        }

        #[test]
        fn keeps_bytes_but_reports_short_descriptor() {
            let fixture = Fixture::new();
            fixture.add("target", nano(), &[0x05, 0x01]);
            let report = capture(&fixture.root(), nano(), Some(285));
            assert_eq!(report["status"], "length_mismatch");
            assert_eq!(report["hex"], "05 01");
        }

        #[test]
        fn rejects_empty_oversize_and_ambiguous_descriptors() {
            let fixture = Fixture::new();
            fixture.add("empty", nano(), &[]);
            assert_eq!(
                capture(&fixture.root(), nano(), None)["status"],
                "unavailable"
            );
            fs::write(
                fixture.0.join("empty/interface/hid/report_descriptor"),
                vec![0; MAX_REPORT_DESCRIPTOR + 1],
            )
            .unwrap();
            let report = capture(&fixture.root(), nano(), None);
            assert_eq!(report["status"], "unavailable");
            assert!(report["reason"].as_str().unwrap().contains("size limit"));
            fixture.add("duplicate", nano(), &[0x05, 0x01]);
            let report = capture(&fixture.root(), nano(), None);
            assert!(report["reason"].as_str().unwrap().contains("ambiguous"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_hid_report_lengths_among_other_descriptors() {
        assert_eq!(
            hid_report_lengths(&[
                3, 0x99, 0, 12, 0x21, 0x11, 0x01, 0, 2, 0x22, 0x1d, 0x01, 0x23, 4, 0,
            ]),
            Ok(vec![285])
        );
    }

    #[test]
    fn rejects_truncated_or_invalid_hid_descriptors() {
        for bytes in [
            &[][..],
            &[0][..],
            &[1, 0x21][..],
            &[9, 0x21, 0x11, 0x01, 0, 1, 0x22, 0x1d][..],
            &[5, 0x21, 0x11, 0x01, 0][..],
            &[9, 0x21, 0x11, 0x01, 0, 2, 0x22, 0x1d, 0x01][..],
        ] {
            assert!(hid_report_lengths(bytes).is_err());
        }
    }
}

//! Read-only HAL device metadata. Do not use CPAL device descriptions here:
//! CPAL 0.17 probes AudioUnits even to produce a device name on macOS.

pub(super) trait Metadata {
    fn devices(&self) -> Vec<u32>;
    fn input_stream_count(&self, device: u32) -> Option<usize>;
    fn name(&self, device: u32) -> Option<String>;
}

pub(super) fn names(metadata: &impl Metadata) -> Vec<String> {
    let mut names: Vec<_> = metadata
        .devices()
        .into_iter()
        .filter(|&device| metadata.input_stream_count(device).unwrap_or(0) > 0)
        .filter_map(|device| metadata.name(device))
        .filter(|name| !name.trim().is_empty())
        .collect();
    // Preserve exact HAL names so the capture worker's selected-name match works.
    names.sort();
    names.dedup();
    names
}

#[cfg(target_os = "macos")]
pub(super) use native::CoreAudio;

#[cfg(target_os = "macos")]
mod native {
    use super::Metadata;
    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::ptr;

    #[repr(C)]
    struct PropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    // AudioHardwareBase.h / AudioHardware.h. These are metadata queries only;
    // they neither create an AudioUnit nor start an input stream.
    const GLOBAL: u32 = u32::from_be_bytes(*b"glob");
    const INPUT: u32 = u32::from_be_bytes(*b"inpt");
    const SYSTEM_OBJECT: u32 = 1;

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        fn AudioObjectGetPropertyDataSize(
            object: u32,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
        ) -> i32;
        fn AudioObjectGetPropertyData(
            object: u32,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
            data: *mut c_void,
        ) -> i32;
    }

    fn address(selector: [u8; 4], scope: u32) -> PropertyAddress {
        PropertyAddress {
            selector: u32::from_be_bytes(selector),
            scope,
            element: 0,
        }
    }

    fn property_size(object: u32, address: &PropertyAddress) -> Option<u32> {
        let mut size = 0;
        // SAFETY: all pointers refer to live fixed-size values for this call.
        let status =
            unsafe { AudioObjectGetPropertyDataSize(object, address, 0, ptr::null(), &mut size) };
        (status == 0).then_some(size)
    }

    pub struct CoreAudio;

    impl Metadata for CoreAudio {
        fn devices(&self) -> Vec<u32> {
            let address = address(*b"dev#", GLOBAL);
            let Some(mut size) = property_size(SYSTEM_OBJECT, &address) else {
                return vec![];
            };
            // Reject malformed metadata before allocating. A hot-plug may also
            // change the list between queries; a failed read is a safe empty list.
            if size == 0 || size % 4 != 0 || size > 4 * 65_536 {
                return vec![];
            }
            let mut devices = vec![0u32; size as usize / size_of::<u32>()];
            let status = unsafe {
                AudioObjectGetPropertyData(
                    SYSTEM_OBJECT,
                    &address,
                    0,
                    ptr::null(),
                    &mut size,
                    devices.as_mut_ptr().cast(),
                )
            };
            if status != 0 || size % 4 != 0 || size as usize > devices.len() * 4 {
                return vec![];
            }
            devices.truncate(size as usize / size_of::<u32>());
            devices
        }

        fn input_stream_count(&self, device: u32) -> Option<usize> {
            // kAudioDevicePropertyStreams in the input scope is an array of
            // AudioStreamIDs. Its size tells us input capability without opening it.
            let size = property_size(device, &address(*b"stm#", INPUT))?;
            (size % 4 == 0).then_some(size as usize / size_of::<u32>())
        }

        fn name(&self, device: u32) -> Option<String> {
            let address = address(*b"lnam", GLOBAL);
            let mut name: CFStringRef = ptr::null();
            let mut size = size_of::<CFStringRef>() as u32;
            let status = unsafe {
                AudioObjectGetPropertyData(
                    device,
                    &address,
                    0,
                    ptr::null(),
                    &mut size,
                    (&mut name as *mut CFStringRef).cast(),
                )
            };
            if status != 0 || name.is_null() {
                return None;
            }
            // kAudioObjectPropertyName transfers ownership of the returned
            // CFString to the caller (AudioHardwareBase.h). The wrapper releases it.
            Some(unsafe { CFString::wrap_under_create_rule(name) }.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeMetadata {
        entries: Vec<(u32, Option<usize>, Option<&'static str>)>,
        names_read: RefCell<Vec<u32>>,
    }
    impl Metadata for FakeMetadata {
        fn devices(&self) -> Vec<u32> {
            self.entries.iter().map(|e| e.0).collect()
        }
        fn input_stream_count(&self, device: u32) -> Option<usize> {
            self.entries.iter().find(|e| e.0 == device).unwrap().1
        }
        fn name(&self, device: u32) -> Option<String> {
            self.names_read.borrow_mut().push(device);
            self.entries
                .iter()
                .find(|e| e.0 == device)
                .unwrap()
                .2
                .map(str::to_owned)
        }
    }

    #[test]
    fn settings_enumerates_inputs_using_only_metadata() {
        let metadata = FakeMetadata {
            entries: vec![
                (1, Some(0), Some("Speakers")),
                (2, Some(1), Some("USB Mic")),
                (3, Some(2), Some("Built-in Mic")),
                (4, Some(1), Some("USB Mic")),
            ],
            names_read: RefCell::default(),
        };
        assert_eq!(names(&metadata), ["Built-in Mic", "USB Mic"]);
        assert_eq!(*metadata.names_read.borrow(), [2, 3, 4]);
    }

    #[test]
    fn disconnected_devices_and_failed_properties_do_not_hide_other_mics() {
        let metadata = FakeMetadata {
            entries: vec![
                (1, None, Some("Disconnected")),
                (2, Some(1), None),
                (3, Some(1), Some("  ")),
                (4, Some(1), Some("Exact name  ")),
            ],
            names_read: RefCell::default(),
        };
        assert_eq!(names(&metadata), ["Exact name  "]);
        assert_eq!(*metadata.names_read.borrow(), [2, 3, 4]);
    }

    /// Explicit, read-only native probe for a machine with pending/denied TCC.
    /// No AudioUnit, stream, permission request, or microphone capture is created.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "reads the host's CoreAudio metadata; run explicitly"]
    fn native_metadata_probe() {
        let started = std::time::Instant::now();
        let devices = names(&CoreAudio);
        eprintln!(
            "CoreAudio metadata: {} input devices in {:?}",
            devices.len(),
            started.elapsed()
        );
        assert!(devices.iter().all(|name| !name.trim().is_empty()));
    }
}

//! Detects whether another application is recording from the virtual cable.
//!
//! Core Audio publishes one process object per client process (macOS 14.2+),
//! with the devices it uses for input and whether its input is running. Audio
//! Switch's own process is ignored, so its render into the cable and its
//! capture from the physical microphone never count as demand.
//!
//! Input state is per process, not per device, and a process's input device
//! list includes every device it does IO on that has inputs. So a process that
//! plays into the cable while recording from another microphone also counts.
//! That only captures when it is not strictly needed; the reverse would leave
//! a recording app silent.
//!
//! Property listeners wake the caller when a process appears or its input
//! changes, so capture can start before the next periodic refresh. The answer
//! itself is always read fresh, so a missed notification only adds latency.

use std::collections::HashSet;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use objc2_core_audio::{
    AudioObjectAddPropertyListener, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectRemovePropertyListener, kAudioDevicePropertyDeviceIsAlive,
    kAudioHardwarePropertyProcessObjectList, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeInput, kAudioObjectPropertyScopeWildcard, kAudioObjectSystemObject,
    kAudioProcessPropertyDevices, kAudioProcessPropertyIsRunningInput, kAudioProcessPropertyPID,
};

use super::AudioError;
use super::coreaudio_backend::{
    address, channel_count, check, device_for_uid, get_property, get_property_array,
};

type ActivityCallback = Arc<dyn Fn() + Send + Sync>;
pub(super) type ActivityWake = Mutex<Option<ActivityCallback>>;

const SYSTEM: AudioObjectID = kAudioObjectSystemObject as AudioObjectID;
/// Process properties whose changes can start or stop recording demand.
const PROCESS_PROPERTIES: [u32; 2] = [
    kAudioProcessPropertyIsRunningInput,
    kAudioProcessPropertyDevices,
];

/// Watches the virtual cable's recording side for other processes.
pub(super) struct ActivityMonitor {
    pub render_id: String,
    pub capture_override: Option<String>,
    device: AudioObjectID,
    wake: &'static ActivityWake,
    /// Process objects carrying our listeners.
    watched: HashSet<AudioObjectID>,
}

impl ActivityMonitor {
    pub fn start(
        render_id: &str,
        capture_override: Option<&str>,
        wake: &'static ActivityWake,
    ) -> Result<Self, AudioError> {
        let device = recording_device(render_id, capture_override)?;
        listen(SYSTEM, kAudioHardwarePropertyProcessObjectList, wake)?;
        Ok(Self {
            render_id: render_id.into(),
            capture_override: capture_override.map(str::to_owned),
            device,
            wake,
            watched: HashSet::new(),
        })
    }

    pub fn in_use(&mut self) -> Result<bool, AudioError> {
        let alive = get_property::<u32>(
            self.device,
            address(
                kAudioDevicePropertyDeviceIsAlive,
                kAudioObjectPropertyScopeGlobal,
            ),
            "read virtual microphone state",
        )
        .is_ok_and(|alive| alive != 0);
        if !alive {
            return Err(AudioError::Backend(
                "virtual microphone recording device is unavailable".into(),
            ));
        }
        let processes: Vec<AudioObjectID> = get_property_array(
            SYSTEM,
            address(
                kAudioHardwarePropertyProcessObjectList,
                kAudioObjectPropertyScopeGlobal,
            ),
            "list audio processes (requires macOS 14.2)",
        )?;
        let own_pid = std::process::id() as i32;
        let mut clients = Vec::with_capacity(processes.len());
        let mut current = HashSet::with_capacity(processes.len());
        for process in processes {
            // A process can exit between listing and inspection.
            let Ok(pid) = get_property::<i32>(
                process,
                address(kAudioProcessPropertyPID, kAudioObjectPropertyScopeGlobal),
                "read process ID",
            ) else {
                continue;
            };
            if pid == own_pid {
                continue;
            }
            current.insert(process);
            // Listen before reading, so a change in between still wakes the caller.
            if self.watched.insert(process) {
                for selector in PROCESS_PROPERTIES {
                    let _ = listen(process, selector, self.wake);
                }
            }
            if let Ok(client) = ClientInput::read(process) {
                clients.push(client);
            }
        }
        // Process objects that are gone take their listeners with them.
        self.watched.retain(|process| current.contains(process));
        Ok(records_from(&clients, self.device))
    }
}

impl Drop for ActivityMonitor {
    fn drop(&mut self) {
        unlisten(SYSTEM, kAudioHardwarePropertyProcessObjectList, self.wake);
        for &process in &self.watched {
            for selector in PROCESS_PROPERTIES {
                unlisten(process, selector, self.wake);
            }
        }
    }
}

/// What one Core Audio client process is doing with input.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClientInput {
    running_input: bool,
    input_devices: Vec<AudioObjectID>,
}

impl ClientInput {
    fn read(process: AudioObjectID) -> Result<Self, AudioError> {
        Ok(Self {
            running_input: get_property::<u32>(
                process,
                address(
                    kAudioProcessPropertyIsRunningInput,
                    kAudioObjectPropertyScopeGlobal,
                ),
                "read process input state",
            )? != 0,
            input_devices: get_property_array(
                process,
                address(kAudioProcessPropertyDevices, kAudioObjectPropertyScopeInput),
                "read process input devices",
            )?,
        })
    }
}

/// Whether any listed process is running input with `device` among its input
/// devices. A process that merely has the cable open, or only records from
/// another device, does not count.
fn records_from(clients: &[ClientInput], device: AudioObjectID) -> bool {
    clients
        .iter()
        .any(|client| client.running_input && client.input_devices.contains(&device))
}

/// The device whose input side applications record from. Core Audio devices
/// carry both directions under one UID, so this is normally the cable itself.
fn recording_device(
    render_id: &str,
    capture_override: Option<&str>,
) -> Result<AudioObjectID, AudioError> {
    let device = device_for_uid(capture_override.unwrap_or(render_id))?;
    if channel_count(device, kAudioObjectPropertyScopeInput)? == 0 {
        return Err(AudioError::Backend(
            if capture_override.is_some() {
                "virtualCapture must be a recording device"
            } else {
                "the virtual cable has no recording side; set endpoints.virtualCapture to its recording device UID in settings.json"
            }
            .into(),
        ));
    }
    Ok(device)
}

fn listen(
    object: AudioObjectID,
    selector: u32,
    wake: &'static ActivityWake,
) -> Result<(), AudioError> {
    let address = address(selector, kAudioObjectPropertyScopeWildcard);
    check(
        unsafe {
            AudioObjectAddPropertyListener(
                object,
                NonNull::from(&address),
                Some(activity_changed),
                wake_pointer(wake),
            )
        },
        "watch audio processes",
    )
}

fn unlisten(object: AudioObjectID, selector: u32, wake: &'static ActivityWake) {
    let address = address(selector, kAudioObjectPropertyScopeWildcard);
    // Fails harmlessly once the process object has gone away.
    unsafe {
        AudioObjectRemovePropertyListener(
            object,
            NonNull::from(&address),
            Some(activity_changed),
            wake_pointer(wake),
        );
    }
}

fn wake_pointer(wake: &'static ActivityWake) -> *mut c_void {
    (wake as *const ActivityWake).cast_mut().cast()
}

/// Runs on a Core Audio notification thread.
unsafe extern "C-unwind" fn activity_changed(
    _object: AudioObjectID,
    _count: u32,
    _addresses: NonNull<AudioObjectPropertyAddress>,
    client_data: *mut c_void,
) -> i32 {
    // SAFETY: listeners are registered with a `&'static ActivityWake`, so it
    // outlives any notification still in flight after removal.
    let wake = unsafe { &*client_data.cast::<ActivityWake>() };
    let callback = wake.lock().ok().and_then(|callback| callback.clone());
    if let Some(callback) = callback {
        callback();
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    const CABLE: AudioObjectID = 7;
    const MIC: AudioObjectID = 3;

    fn client(running_input: bool, input_devices: &[AudioObjectID]) -> ClientInput {
        ClientInput {
            running_input,
            input_devices: input_devices.to_vec(),
        }
    }

    #[test]
    fn only_running_input_from_the_cable_is_demand() {
        assert!(!records_from(&[], CABLE));
        // Opened the cable for input but stopped, as an idle meeting app does.
        assert!(!records_from(&[client(false, &[CABLE])], CABLE));
        // Recording, but from another microphone.
        assert!(!records_from(&[client(true, &[MIC])], CABLE));
        assert!(records_from(
            &[client(true, &[MIC]), client(true, &[MIC, CABLE])],
            CABLE
        ));
    }
}

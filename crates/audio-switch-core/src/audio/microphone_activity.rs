use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_InstanceId;
use windows::Win32::Media::Audio::{
    AudioSessionDisconnectReason, AudioSessionState, AudioSessionStateActive,
    AudioSessionStateExpired, DEVICE_STATE_ACTIVE, IAudioSessionControl, IAudioSessionControl2,
    IAudioSessionEvents, IAudioSessionEvents_Impl, IAudioSessionManager2,
    IAudioSessionNotification, IAudioSessionNotification_Impl, IDeviceTopology, IMMDevice,
    IMMDeviceEnumerator, IMMEndpoint, MMDeviceEnumerator, eCapture,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ};
use windows::core::{AgileReference, BOOL, GUID, Interface, PCWSTR, PWSTR, Ref, implement};

use super::AudioError;

type ActivityCallback = Arc<dyn Fn() + Send + Sync>;
pub(super) type ActivityWake = Arc<Mutex<Option<ActivityCallback>>>;

pub(super) struct ActivityMonitor {
    pub render_id: String,
    pub capture_override: Option<String>,
    state: Arc<Mutex<Result<bool, String>>>,
    events: mpsc::Sender<Event>,
    worker: Option<JoinHandle<()>>,
}

impl ActivityMonitor {
    pub fn start(
        render_id: &str,
        capture_override: Option<&str>,
        wake: ActivityWake,
    ) -> Result<Self, AudioError> {
        let state = Arc::new(Mutex::new(Ok(false)));
        let (events, receiver) = mpsc::channel();
        let (ready, startup) = mpsc::sync_channel(1);
        let worker_state = Arc::clone(&state);
        let worker_events = events.clone();
        let worker_render = render_id.to_owned();
        let worker_capture = capture_override.map(str::to_owned);
        let worker = thread::Builder::new()
            .name("audio-switch-mic-activity".into())
            .spawn(move || {
                let mut initialized = false;
                let result = super::wasapi_backend::with_com(|| {
                    let receiver = receiver;
                    let mut sessions = SessionMonitor::new(
                        &worker_render,
                        worker_capture.as_deref(),
                        worker_events,
                    )?;
                    publish(&worker_state, Ok(sessions.in_use()?), &wake);
                    initialized = true;
                    let _ = ready.send(Ok(()));
                    loop {
                        match receiver.recv_timeout(Duration::from_secs(1)) {
                            Ok(Event::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            Ok(Event::Created(session)) => {
                                sessions.add(session.resolve().map_err(api_error)?)?
                            }
                            Ok(Event::Failed(message)) => return Err(AudioError::Backend(message)),
                            Ok(Event::Changed) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                        }
                        publish(&worker_state, Ok(sessions.in_use()?), &wake);
                    }
                    Ok(())
                });
                if let Err(error) = result {
                    let message = error.to_string();
                    if initialized {
                        publish(&worker_state, Err(message), &wake);
                    } else {
                        let _ = ready.send(Err(message));
                    }
                }
            })
            .map_err(|error| AudioError::Backend(error.to_string()))?;
        let monitor = Self {
            render_id: render_id.into(),
            capture_override: capture_override.map(str::to_owned),
            state,
            events,
            worker: Some(worker),
        };
        startup
            .recv_timeout(Duration::from_secs(8))
            .map_err(|error| AudioError::Backend(format!("microphone activity monitor: {error}")))?
            .map_err(AudioError::Backend)?;
        Ok(monitor)
    }

    pub fn in_use(&self) -> Result<bool, AudioError> {
        self.state
            .lock()
            .expect("activity mutex poisoned")
            .clone()
            .map_err(AudioError::Backend)
    }
}

impl Drop for ActivityMonitor {
    fn drop(&mut self) {
        let _ = self.events.send(Event::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn publish(state: &Mutex<Result<bool, String>>, next: Result<bool, String>, wake: &ActivityWake) {
    let changed = {
        let mut current = state.lock().expect("activity mutex poisoned");
        if *current == next {
            false
        } else {
            *current = next;
            true
        }
    };
    let callback = if changed {
        wake.lock()
            .expect("activity callback mutex poisoned")
            .clone()
    } else {
        None
    };
    if let Some(callback) = callback {
        callback();
    }
}

enum Event {
    Created(AgileReference<IAudioSessionControl>),
    Failed(String),
    Changed,
    Stop,
}

struct SessionMonitor {
    device: IMMDevice,
    manager: IAudioSessionManager2,
    notification: IAudioSessionNotification,
    events: mpsc::Sender<Event>,
    sessions: HashMap<String, Session>,
}

impl SessionMonitor {
    fn new(
        render_id: &str,
        capture_override: Option<&str>,
        events: mpsc::Sender<Event>,
    ) -> Result<Self, AudioError> {
        let device = capture_device(render_id, capture_override)?;
        let manager: IAudioSessionManager2 =
            unsafe { device.Activate(CLSCTX_ALL, None) }.map_err(api_error)?;
        let notification: IAudioSessionNotification = SessionCreated {
            events: events.clone(),
        }
        .into();
        unsafe { manager.RegisterSessionNotification(&notification) }.map_err(api_error)?;
        let mut monitor = Self {
            device,
            manager,
            notification,
            events,
            sessions: HashMap::new(),
        };
        let enumerator = unsafe { monitor.manager.GetSessionEnumerator() }.map_err(api_error)?;
        // GetCount enables new-session callbacks. Retain both enumerated and notified sessions.
        let count = unsafe { enumerator.GetCount() }.map_err(api_error)?;
        for index in 0..count {
            monitor.add(unsafe { enumerator.GetSession(index) }.map_err(api_error)?)?;
        }
        Ok(monitor)
    }

    fn add(&mut self, control: IAudioSessionControl) -> Result<(), AudioError> {
        let extended: IAudioSessionControl2 = control.cast().map_err(api_error)?;
        let id =
            take_string(unsafe { extended.GetSessionInstanceIdentifier() }.map_err(api_error)?)?;
        if self.sessions.contains_key(&id) {
            return Ok(());
        }
        let events: IAudioSessionEvents = SessionEvents {
            events: self.events.clone(),
        }
        .into();
        unsafe { control.RegisterAudioSessionNotification(&events) }.map_err(api_error)?;
        self.sessions.insert(id, Session { control, events });
        Ok(())
    }

    fn in_use(&mut self) -> Result<bool, AudioError> {
        if unsafe { self.device.GetState() }.map_err(api_error)? != DEVICE_STATE_ACTIVE {
            return Err(AudioError::Backend(
                "virtual microphone recording endpoint is unavailable".into(),
            ));
        }
        let mut active = false;
        let mut expired = Vec::new();
        for (id, session) in &self.sessions {
            let state = unsafe { session.control.GetState() }.map_err(api_error)?;
            active |= state == AudioSessionStateActive;
            if state == AudioSessionStateExpired {
                expired.push(id.clone());
            }
        }
        for id in expired {
            self.sessions.remove(&id);
        }
        Ok(active)
    }
}

impl Drop for SessionMonitor {
    fn drop(&mut self) {
        let _ = unsafe {
            self.manager
                .UnregisterSessionNotification(&self.notification)
        };
    }
}

struct Session {
    control: IAudioSessionControl,
    events: IAudioSessionEvents,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = unsafe {
            self.control
                .UnregisterAudioSessionNotification(&self.events)
        };
    }
}

fn capture_device(
    render_id: &str,
    capture_override: Option<&str>,
) -> Result<IMMDevice, AudioError> {
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(api_error)?;
    if let Some(id) = capture_override {
        let device = get_device(&enumerator, id)?;
        let endpoint: IMMEndpoint = device.cast().map_err(api_error)?;
        if unsafe { endpoint.GetDataFlow() }.map_err(api_error)? != eCapture {
            return Err(AudioError::Backend(
                "virtualCapture must be a recording endpoint".into(),
            ));
        }
        return Ok(device);
    }
    let adapter = adapter_id(&enumerator, &get_device(&enumerator, render_id)?)?;
    let devices = unsafe { enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) }
        .map_err(api_error)?;
    let mut matches = Vec::new();
    for index in 0..unsafe { devices.GetCount() }.map_err(api_error)? {
        let device = unsafe { devices.Item(index) }.map_err(api_error)?;
        if adapter_id(&enumerator, &device)
            .is_ok_and(|candidate| candidate.eq_ignore_ascii_case(&adapter))
        {
            matches.push(device);
        }
    }
    if matches.len() != 1 {
        return Err(AudioError::Backend(format!(
            "found {} recording endpoints for the virtual cable; set endpoints.virtualCapture to its recording endpoint ID in settings.json",
            matches.len()
        )));
    }
    Ok(matches.pop().expect("one matching recording endpoint"))
}

fn get_device(enumerator: &IMMDeviceEnumerator, id: &str) -> Result<IMMDevice, AudioError> {
    let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
    unsafe { enumerator.GetDevice(PCWSTR(wide.as_ptr())) }.map_err(api_error)
}

fn adapter_id(enumerator: &IMMDeviceEnumerator, device: &IMMDevice) -> Result<String, AudioError> {
    let topology: IDeviceTopology =
        unsafe { device.Activate(CLSCTX_ALL, None) }.map_err(api_error)?;
    let connector = unsafe { topology.GetConnector(0) }.map_err(api_error)?;
    let filter_id = take_string(unsafe { connector.GetDeviceIdConnectedTo() }.map_err(api_error)?)?;
    let filter = get_device(enumerator, &filter_id)?;
    let properties = unsafe { filter.OpenPropertyStore(STGM_READ) }.map_err(api_error)?;
    let mut value = unsafe { properties.GetValue(&PKEY_Device_InstanceId) }.map_err(api_error)?;
    let string = unsafe { PropVariantToStringAlloc(&value) }.map_err(api_error);
    let cleared = unsafe { PropVariantClear(&mut value) }.map_err(api_error);
    let id = take_string(string?)?;
    cleared?;
    if id.is_empty() {
        return Err(AudioError::Backend(
            "audio adapter has no device instance ID".into(),
        ));
    }
    Ok(id)
}

fn take_string(value: PWSTR) -> Result<String, AudioError> {
    let result = unsafe { value.to_string() };
    unsafe { CoTaskMemFree(Some(value.0.cast())) };
    result.map_err(|error| AudioError::Backend(error.to_string()))
}

fn api_error(error: windows::core::Error) -> AudioError {
    AudioError::Backend(format!("microphone activity: {error}"))
}

#[implement(IAudioSessionNotification)]
struct SessionCreated {
    events: mpsc::Sender<Event>,
}

impl IAudioSessionNotification_Impl for SessionCreated_Impl {
    fn OnSessionCreated(&self, session: Ref<IAudioSessionControl>) -> windows::core::Result<()> {
        if let Some(session) = session.as_ref() {
            let event = match AgileReference::new(session) {
                Ok(session) => Event::Created(session),
                Err(error) => Event::Failed(error.to_string()),
            };
            let _ = self.events.send(event);
        }
        Ok(())
    }
}

#[implement(IAudioSessionEvents)]
struct SessionEvents {
    events: mpsc::Sender<Event>,
}

impl IAudioSessionEvents_Impl for SessionEvents_Impl {
    fn OnStateChanged(&self, _: AudioSessionState) -> windows::core::Result<()> {
        let _ = self.events.send(Event::Changed);
        Ok(())
    }
    fn OnSessionDisconnected(&self, _: AudioSessionDisconnectReason) -> windows::core::Result<()> {
        let _ = self.events.send(Event::Changed);
        Ok(())
    }
    fn OnDisplayNameChanged(&self, _: &PCWSTR, _: *const GUID) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnIconPathChanged(&self, _: &PCWSTR, _: *const GUID) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnSimpleVolumeChanged(&self, _: f32, _: BOOL, _: *const GUID) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnChannelVolumeChanged(
        &self,
        _: u32,
        _: *const f32,
        _: u32,
        _: *const GUID,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnGroupingParamChanged(&self, _: *const GUID, _: *const GUID) -> windows::core::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Media::Audio::{AUDCLNT_SHAREMODE_SHARED, IAudioClient};

    #[test]
    fn failed_monitor_start_does_not_trigger_an_immediate_retry_loop() {
        let (notify, changes) = mpsc::channel();
        let wake: ActivityWake = Arc::new(Mutex::new(Some(Arc::new(move || {
            let _ = notify.send(());
        }))));
        assert!(ActivityMonitor::start("missing-render-endpoint", None, wake).is_err());
        assert!(changes.try_recv().is_err());
    }

    fn recording_client(device: &IMMDevice, session_id: u128) -> IAudioClient {
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }.unwrap();
        let format = unsafe { client.GetMixFormat() }.unwrap();
        let initialized = unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                0,
                1_000_000,
                0,
                format,
                Some(&GUID::from_u128(session_id)),
            )
        };
        unsafe { CoTaskMemFree(Some(format.cast())) };
        initialized.unwrap();
        client
    }

    fn expect_activity(monitor: &ActivityMonitor, changes: &mpsc::Receiver<()>, expected: bool) {
        let started = std::time::Instant::now();
        changes
            .recv_timeout(Duration::from_millis(750))
            .expect("session callback must wake capture before the one-second health poll");
        assert_eq!(monitor.in_use().unwrap(), expected);
        println!(
            "Recording demand {expected}, notification received after {:?}",
            started.elapsed()
        );
    }

    #[test]
    #[ignore = "opens temporary virtual-cable recording streams; requires AUDIO_SWITCH_TEST_VIRTUAL_RENDER and no other cable consumers"]
    fn native_capture_sessions_start_stop_and_overlap() {
        let render_id = std::env::var("AUDIO_SWITCH_TEST_VIRTUAL_RENDER").unwrap();
        super::super::wasapi_backend::with_com(|| {
            let device = capture_device(&render_id, None)?;
            let first = recording_client(&device, 1);
            unsafe { first.Start() }.unwrap();
            let (notify, changes) = mpsc::channel();
            let wake: ActivityWake = Arc::new(Mutex::new(Some(Arc::new(move || {
                let _ = notify.send(());
            }))));
            let monitor = ActivityMonitor::start(&render_id, None, wake)?;
            assert!(monitor.in_use()?);
            changes.recv_timeout(Duration::from_millis(750)).unwrap();

            unsafe { first.Stop() }.unwrap();
            expect_activity(&monitor, &changes, false);
            unsafe { first.Start() }.unwrap();
            expect_activity(&monitor, &changes, true);

            let second = recording_client(&device, 2);
            unsafe { second.Start() }.unwrap();
            thread::sleep(Duration::from_millis(100));
            unsafe { first.Stop() }.unwrap();
            thread::sleep(Duration::from_millis(100));
            assert!(monitor.in_use()?);
            assert!(changes.try_recv().is_err());
            unsafe { second.Stop() }.unwrap();
            expect_activity(&monitor, &changes, false);
            drop(first);
            drop(second);

            let third = recording_client(&device, 3);
            unsafe { third.Start() }.unwrap();
            expect_activity(&monitor, &changes, true);
            unsafe { third.Stop() }.unwrap();
            expect_activity(&monitor, &changes, false);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    #[ignore = "reads Windows audio sessions; requires AUDIO_SWITCH_TEST_VIRTUAL_RENDER"]
    fn native_cable_pair_and_activity() {
        let render_id = std::env::var("AUDIO_SWITCH_TEST_VIRTUAL_RENDER").unwrap();
        super::super::wasapi_backend::with_com(|| {
            let device = capture_device(&render_id, None)?;
            let id = take_string(unsafe { device.GetId() }.map_err(api_error)?)?;
            println!("Paired recording endpoint: {id}");
            Ok(())
        })
        .unwrap();
        let monitor = ActivityMonitor::start(&render_id, None, Arc::new(Mutex::new(None))).unwrap();
        println!(
            "Applications recording from cable: {}",
            monitor.in_use().unwrap()
        );
    }
}

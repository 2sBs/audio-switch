//! macOS backend built on Core Audio HAL IOProcs.
//!
//! Each route runs one IOProc on the source device and one on the sink device.
//! The shared ring and `AdaptiveReader` bridge the two clocks and any sample
//! rate difference, so no device's nominal rate is ever changed. Playback
//! mirroring reads the Primary device through a private process tap (macOS
//! 14.2+) exposed as the input of a private aggregate device.

use std::ffi::{CStr, c_void};
use std::mem::{MaybeUninit, size_of};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart,
    AudioDeviceStop, AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap,
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, AudioObjectPropertyScope, AudioObjectPropertySelector,
    CATapDescription, CATapMuteBehavior, kAudioAggregateDeviceIsPrivateKey,
    kAudioAggregateDeviceNameKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey,
    kAudioDevicePropertyDeviceIsAlive, kAudioDevicePropertyDeviceUID, kAudioDevicePropertyIsHidden,
    kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertyStreamConfiguration,
    kAudioDevicePropertyStreams, kAudioHardwarePropertyDevices,
    kAudioHardwarePropertyTranslateUIDToDevice, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput,
    kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject, kAudioObjectUnknown,
    kAudioStreamPropertyVirtualFormat, kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey,
    kAudioTapPropertyFormat,
};
use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp,
    kAudioFormatFlagIsFloat, kAudioFormatLinearPCM,
};
use objc2_core_foundation::{CFDictionary, CFRetained, CFString};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSUUID};
use rtrb::Producer;

use crate::state::ActiveMic;

use super::bridge::{AdaptiveReader, AtomicDiagnostics, RouteDiagnostics, frames_for_ms, ring};
use super::{AudioBackend, AudioError, CueKind, Endpoint, EndpointFlow, synthesize_cue};

/// UID prefix of the private aggregate devices this process creates. They are
/// visible to this process only and never offered as endpoints.
const AGGREGATE_UID_PREFIX: &str = "com.2sbsi.audio-switch.";
/// Largest block rendered from the ring at once; IOProc buffers larger than
/// this are filled in several passes so the render path never allocates.
const RENDER_CHUNK_FRAMES: usize = 4_096;
const CUE_TAIL: Duration = Duration::from_millis(30);
const START_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct CoreAudioBackend {
    mirror_buffer_ms: u32,
    mic_buffer_ms: u32,
    mirror: Mutex<Option<Route>>,
    microphone: Mutex<Option<Route>>,
}

impl CoreAudioBackend {
    pub fn new(mirror_buffer_ms: u32, mic_buffer_ms: u32) -> Self {
        Self {
            mirror_buffer_ms,
            mic_buffer_ms,
            mirror: Mutex::new(None),
            microphone: Mutex::new(None),
        }
    }

    pub fn mirror_diagnostics(&self) -> Option<RouteDiagnostics> {
        Self::diagnostics(&self.mirror)
    }

    pub fn microphone_diagnostics(&self) -> Option<RouteDiagnostics> {
        Self::diagnostics(&self.microphone)
    }

    fn diagnostics(slot: &Mutex<Option<Route>>) -> Option<RouteDiagnostics> {
        slot.lock()
            .expect("route mutex poisoned")
            .as_ref()
            .map(|route| route.diagnostics.snapshot())
    }

    /// Starts the new route before stopping the previous one, as the Windows
    /// backend does, so switching microphones does not open a silent gap while
    /// the new input device wakes up. If the new route fails, the previous
    /// route keeps running.
    fn replace_route(
        slot: &Mutex<Option<Route>>,
        start: impl FnOnce() -> Result<Route, AudioError>,
    ) -> Result<(), AudioError> {
        let mut slot = slot.lock().expect("route mutex poisoned");
        // The new route is fully started before assignment drops the old one.
        *slot = Some(start()?);
        Ok(())
    }
}

impl AudioBackend for CoreAudioBackend {
    fn enumerate_endpoints(&self) -> Result<Vec<Endpoint>, AudioError> {
        let devices: Vec<AudioObjectID> = get_property_array(
            kAudioObjectSystemObject as AudioObjectID,
            address(
                kAudioHardwarePropertyDevices,
                kAudioObjectPropertyScopeGlobal,
            ),
            "list audio devices",
        )?;
        let mut endpoints = Vec::new();
        for device in devices {
            // Devices can disappear between listing and inspection.
            if let Ok(Some(device_endpoints)) = describe_device(device) {
                endpoints.extend(device_endpoints);
            }
        }
        endpoints.sort_by(|left, right| {
            left.flow
                .cmp(&right.flow)
                .then_with(|| left.name.cmp(&right.name))
        });
        Ok(endpoints)
    }

    fn start_mirror(
        &self,
        source_render_id: &str,
        target_render_id: &str,
    ) -> Result<(), AudioError> {
        Self::replace_route(&self.mirror, || {
            let sink = device_for_uid(target_render_id)?;
            ensure_float_streams(sink, kAudioObjectPropertyScopeOutput)?;
            let playback = PlaybackTap::open(source_render_id)?;
            Route::start(RouteSpecification {
                source: playback.aggregate.id,
                source_channels: playback.channels.clone(),
                sink,
                buffer_ms: self.mirror_buffer_ms,
                playback_tap: Some(playback),
            })
        })
    }

    fn stop_mirror(&self) -> Result<(), AudioError> {
        self.mirror.lock().expect("mirror mutex poisoned").take();
        Ok(())
    }

    fn start_microphone(
        &self,
        source_capture_id: &str,
        _active: ActiveMic,
        sink_render_id: &str,
    ) -> Result<(), AudioError> {
        Self::replace_route(&self.microphone, || {
            let source = device_for_uid(source_capture_id)?;
            let sink = device_for_uid(sink_render_id)?;
            ensure_float_streams(source, kAudioObjectPropertyScopeInput)?;
            ensure_float_streams(sink, kAudioObjectPropertyScopeOutput)?;
            Route::start(RouteSpecification {
                source,
                // Like the Windows backend, the first capture channel is the voice.
                source_channels: vec![0],
                sink,
                buffer_ms: self.mic_buffer_ms,
                playback_tap: None,
            })
        })
    }

    fn stop_microphone(&self) -> Result<(), AudioError> {
        self.microphone
            .lock()
            .expect("microphone mutex poisoned")
            .take();
        Ok(())
    }

    fn play_cue(
        &self,
        render_endpoint_id: &str,
        cue: CueKind,
        level_db: f32,
    ) -> Result<(), AudioError> {
        let device = device_for_uid(render_endpoint_id)?;
        ensure_float_streams(device, kAudioObjectPropertyScopeOutput)?;
        let sample_rate = nominal_sample_rate(device)?;
        let samples = synthesize_cue(cue, sample_rate.round() as u32, level_db);
        let duration = Duration::from_secs_f64(samples.len() as f64 / sample_rate);
        let finished = Arc::new(AtomicBool::new(false));
        let _proc = IoProc::start(
            device,
            CueContext {
                samples,
                position: 0,
                finished: Arc::clone(&finished),
            },
            "start cue playback",
        )?;
        let deadline = Instant::now() + duration + Duration::from_secs(1);
        while !finished.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        // Allow the final IO cycle to reach the hardware before stopping.
        thread::sleep(CUE_TAIL);
        Ok(())
    }
}

struct RouteSpecification {
    source: AudioObjectID,
    /// Source channel feeding each ring channel.
    source_channels: Vec<usize>,
    sink: AudioObjectID,
    buffer_ms: u32,
    playback_tap: Option<PlaybackTap>,
}

/// A running route. Fields drop in declaration order: both IOProcs stop before
/// the playback tap they read from is torn down.
#[derive(Debug)]
struct Route {
    _capture: IoProc<CaptureContext>,
    _render: IoProc<RenderContext>,
    _playback_tap: Option<PlaybackTap>,
    diagnostics: Arc<AtomicDiagnostics>,
}

impl Route {
    fn start(specification: RouteSpecification) -> Result<Self, AudioError> {
        let source_rate = nominal_sample_rate(specification.source)?;
        let sink_rate = nominal_sample_rate(specification.sink)?;
        let ring_channels = specification.source_channels.len();
        let target_frames = frames_for_ms(source_rate.round() as u32, specification.buffer_ms);
        let (producer, consumer) = ring(target_frames, ring_channels);
        let diagnostics = Arc::new(AtomicDiagnostics::default());

        let render = IoProc::start(
            specification.sink,
            RenderContext {
                reader: AdaptiveReader::new(
                    consumer,
                    ring_channels,
                    target_frames,
                    source_rate / sink_rate,
                    Arc::clone(&diagnostics),
                ),
                ring_channels,
                scratch: vec![0.0; RENDER_CHUNK_FRAMES * ring_channels],
                diagnostics: Arc::clone(&diagnostics),
            },
            "start output device",
        )?;
        let capture = IoProc::start(
            specification.source,
            CaptureContext {
                producer,
                source_channels: specification.source_channels,
                diagnostics: Arc::clone(&diagnostics),
            },
            "start input device",
        )?;

        // Input devices can take hundreds of milliseconds to deliver their
        // first buffer. Report the route as started only once it carries
        // audio, so a replaced route keeps playing until then.
        let deadline = Instant::now() + START_TIMEOUT;
        while diagnostics.captured_frames.load(Ordering::Relaxed) < target_frames as u64 {
            if Instant::now() >= deadline {
                return Err(AudioError::Backend(format!(
                    "input device delivered no audio within {} seconds",
                    START_TIMEOUT.as_secs()
                )));
            }
            thread::sleep(Duration::from_millis(5));
        }
        Ok(Self {
            _capture: capture,
            _render: render,
            _playback_tap: specification.playback_tap,
            diagnostics,
        })
    }
}

/// State owned by one IOProc and touched only from the device's IO thread.
trait IoContext: Send + 'static {
    /// Runs on a real-time thread: must not block or allocate.
    fn process(&mut self, input: &AudioBufferList, output: &mut AudioBufferList);
}

/// A registered, started IOProc. Dropping it stops and unregisters the IOProc
/// before freeing the context the callback reads.
#[derive(Debug)]
struct IoProc<C: IoContext> {
    device: AudioObjectID,
    id: AudioDeviceIOProcID,
    context: NonNull<C>,
}

// SAFETY: the context is only dereferenced by the HAL IO thread while the
// IOProc is registered, and freed after it has been destroyed.
unsafe impl<C: IoContext> Send for IoProc<C> {}

impl<C: IoContext> IoProc<C> {
    fn start(device: AudioObjectID, context: C, stage: &str) -> Result<Self, AudioError> {
        let context = NonNull::from(Box::leak(Box::new(context)));
        let mut id: AudioDeviceIOProcID = None;
        let status = unsafe {
            AudioDeviceCreateIOProcID(
                device,
                Some(io_proc::<C>),
                context.as_ptr().cast(),
                NonNull::from(&mut id),
            )
        };
        if let Err(error) = check(status, stage) {
            drop(unsafe { Box::from_raw(context.as_ptr()) });
            return Err(error);
        }
        let proc = Self {
            device,
            id,
            context,
        };
        check(unsafe { AudioDeviceStart(device, proc.id) }, stage)?;
        Ok(proc)
    }
}

impl<C: IoContext> Drop for IoProc<C> {
    fn drop(&mut self) {
        // Both calls fail harmlessly when the device has already gone away.
        unsafe {
            AudioDeviceStop(self.device, self.id);
            AudioDeviceDestroyIOProcID(self.device, self.id);
            drop(Box::from_raw(self.context.as_ptr()));
        }
    }
}

unsafe extern "C-unwind" fn io_proc<C: IoContext>(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client_data: *mut c_void,
) -> i32 {
    // SAFETY: `client_data` is the context owned by `IoProc<C>`, which outlives
    // the registration. The HAL calls an IOProc serially, so access is unique.
    unsafe {
        let context = &mut *client_data.cast::<C>();
        context.process(input.as_ref(), &mut *output.as_ptr());
    }
    0
}

#[derive(Debug)]
struct CaptureContext {
    producer: Producer<f32>,
    source_channels: Vec<usize>,
    diagnostics: Arc<AtomicDiagnostics>,
}

impl IoContext for CaptureContext {
    fn process(&mut self, input: &AudioBufferList, _output: &mut AudioBufferList) {
        let buffers = buffers(input);
        let frames = frame_count(buffers);
        self.diagnostics
            .captured_frames
            .fetch_add(frames as u64, Ordering::Relaxed);
        for frame in 0..frames {
            if self.producer.slots() < self.source_channels.len() {
                self.diagnostics
                    .overrun_frames
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            }
            for &channel in &self.source_channels {
                self.producer
                    .push(sample_at(buffers, channel, frame))
                    .expect("capacity was checked for the complete frame");
            }
        }
    }
}

#[derive(Debug)]
struct RenderContext {
    reader: AdaptiveReader,
    ring_channels: usize,
    scratch: Vec<f32>,
    diagnostics: Arc<AtomicDiagnostics>,
}

impl IoContext for RenderContext {
    fn process(&mut self, _input: &AudioBufferList, output: &mut AudioBufferList) {
        let buffers = buffers_mut(output);
        let frames = frame_count(buffers);
        let mut offset = 0;
        while offset < frames {
            let chunk = (frames - offset).min(RENDER_CHUNK_FRAMES);
            let scratch = &mut self.scratch[..chunk * self.ring_channels];
            self.reader.render(scratch);
            let mut first_channel = 0;
            for buffer in buffers.iter_mut() {
                let channels = buffer.mNumberChannels as usize;
                let samples = buffer_samples_mut(buffer);
                for channel in 0..channels {
                    // Mono fans out to every output channel; stereo fills the
                    // first two and leaves the rest silent.
                    let ring_channel = match self.ring_channels {
                        1 => 0,
                        ring_channels if first_channel + channel < ring_channels => {
                            first_channel + channel
                        }
                        _ => continue,
                    };
                    for frame in 0..chunk {
                        if let Some(sample) = samples.get_mut((offset + frame) * channels + channel)
                        {
                            *sample = scratch[frame * self.ring_channels + ring_channel];
                        }
                    }
                }
                first_channel += channels;
            }
            offset += chunk;
        }
        self.diagnostics
            .rendered_frames
            .fetch_add(frames as u64, Ordering::Relaxed);
    }
}

struct CueContext {
    samples: Vec<f32>,
    position: usize,
    finished: Arc<AtomicBool>,
}

impl IoContext for CueContext {
    fn process(&mut self, _input: &AudioBufferList, output: &mut AudioBufferList) {
        let buffers = buffers_mut(output);
        let frames = frame_count(buffers);
        let available = frames.min(self.samples.len() - self.position);
        let cue = &self.samples[self.position..self.position + available];
        for buffer in buffers.iter_mut() {
            let channels = buffer.mNumberChannels as usize;
            for (frame, output) in buffer_samples_mut(buffer)
                .chunks_exact_mut(channels.max(1))
                .zip(cue)
            {
                frame.fill(*output);
            }
        }
        self.position += available;
        if self.position == self.samples.len() {
            self.finished.store(true, Ordering::Release);
        }
    }
}

/// Exposes everything an output device plays as the input of an aggregate
/// device. Fields drop in declaration order: the aggregate goes before the tap
/// it reads, and the tapped device's hardware stops last.
#[derive(Debug)]
struct PlaybackTap {
    aggregate: AggregateDevice,
    _tap: ProcessTap,
    _running: RunningDevice,
    /// Aggregate input channels that carry the tapped playback.
    channels: Vec<usize>,
}

impl PlaybackTap {
    fn open(device_uid: &str) -> Result<Self, AudioError> {
        // A tap only produces audio while its device runs, and starting the
        // aggregate does not start an idle tapped device.
        let running = RunningDevice::start(device_for_uid(device_uid)?)?;
        let tap = ProcessTap::for_output_device(device_uid)?;
        let aggregate = AggregateDevice::for_tap(&tap.uid)?;
        ensure_float_streams(aggregate.id, kAudioObjectPropertyScopeInput)?;
        let format: AudioStreamBasicDescription = get_property(
            tap.id,
            address(kAudioTapPropertyFormat, kAudioObjectPropertyScopeGlobal),
            "read playback tap format",
        )?;
        let channels = match format.mChannelsPerFrame {
            0 => {
                return Err(AudioError::UnsupportedFormat(
                    "playback tap has no channels".into(),
                ));
            }
            1 => vec![0, 0],
            _ => vec![0, 1],
        };
        Ok(Self {
            aggregate,
            _tap: tap,
            _running: running,
            channels,
        })
    }
}

/// Keeps a device's hardware running without registering an IOProc.
#[derive(Debug)]
struct RunningDevice(AudioObjectID);

impl RunningDevice {
    fn start(device: AudioObjectID) -> Result<Self, AudioError> {
        check(
            unsafe { AudioDeviceStart(device, None) },
            "start tapped output device",
        )?;
        Ok(Self(device))
    }
}

impl Drop for RunningDevice {
    fn drop(&mut self) {
        unsafe {
            AudioDeviceStop(self.0, None);
        }
    }
}

/// A private process tap on everything an output device plays, including this
/// process's own cues, as Windows loopback capture does.
#[derive(Debug)]
struct ProcessTap {
    id: AudioObjectID,
    uid: String,
}

impl ProcessTap {
    fn for_output_device(device_uid: &str) -> Result<Self, AudioError> {
        let description = unsafe {
            CATapDescription::initExcludingProcesses_andDeviceUID_withStream(
                CATapDescription::alloc(),
                &NSArray::new(),
                &NSString::from_str(device_uid),
                0,
            )
        };
        unsafe {
            description.setName(&NSString::from_str("Audio Switch playback mirror"));
            description.setPrivate(true);
            description.setMuteBehavior(CATapMuteBehavior::Unmuted);
        }
        let mut id = kAudioObjectUnknown;
        check(
            unsafe { AudioHardwareCreateProcessTap(Some(&description), &mut id) },
            "create playback tap (requires macOS 14.2 and System Audio Recording access)",
        )?;
        let uid = unsafe { description.UUID().UUIDString() }.to_string();
        Ok(Self { id, uid })
    }
}

impl Drop for ProcessTap {
    fn drop(&mut self) {
        unsafe {
            AudioHardwareDestroyProcessTap(self.id);
        }
    }
}

/// A private aggregate device whose only input is a process tap. Taps can only
/// be read through an aggregate device. It has no sub-devices, so mirroring a
/// device that also has inputs never opens its microphone.
#[derive(Debug)]
struct AggregateDevice {
    id: AudioObjectID,
}

impl AggregateDevice {
    fn for_tap(tap_uid: &str) -> Result<Self, AudioError> {
        let tap_keys = [
            &*cf_key(kAudioSubTapUIDKey),
            &*cf_key(kAudioSubTapDriftCompensationKey),
        ];
        let tap_uid = NSString::from_str(tap_uid);
        let enabled = NSNumber::new_bool(true);
        let tap_values: [&AnyObject; 2] = [&tap_uid, &enabled];
        let tap = NSDictionary::<NSString, AnyObject>::from_slices(&tap_keys, &tap_values);
        let taps = NSArray::from_retained_slice(&[tap]);

        let name = NSString::from_str("Audio Switch playback mirror");
        let uid = NSString::from_str(&format!(
            "{AGGREGATE_UID_PREFIX}mirror.{}",
            NSUUID::new().UUIDString()
        ));
        let keys = [
            &*cf_key(kAudioAggregateDeviceNameKey),
            &*cf_key(kAudioAggregateDeviceUIDKey),
            &*cf_key(kAudioAggregateDeviceIsPrivateKey),
            &*cf_key(kAudioAggregateDeviceTapListKey),
            &*cf_key(kAudioAggregateDeviceTapAutoStartKey),
        ];
        let values: [&AnyObject; 5] = [&name, &uid, &enabled, &taps, &enabled];

        let description = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &values);

        let mut id = kAudioObjectUnknown;
        // SAFETY: NSDictionary is toll-free bridged with CFDictionary.
        let description = unsafe { &*Retained::as_ptr(&description).cast::<CFDictionary>() };
        check(
            unsafe { AudioHardwareCreateAggregateDevice(description, NonNull::from(&mut id)) },
            "create playback mirror device",
        )?;
        Ok(Self { id })
    }
}

impl Drop for AggregateDevice {
    fn drop(&mut self) {
        unsafe {
            AudioHardwareDestroyAggregateDevice(self.id);
        }
    }
}

fn cf_key(key: &CStr) -> Retained<NSString> {
    NSString::from_str(key.to_str().expect("Core Audio keys are ASCII"))
}

fn describe_device(device: AudioObjectID) -> Result<Option<Vec<Endpoint>>, AudioError> {
    let global = kAudioObjectPropertyScopeGlobal;
    let id = get_string(device, address(kAudioDevicePropertyDeviceUID, global))?;
    let hidden = get_property::<u32>(device, address(kAudioDevicePropertyIsHidden, global), "")
        .is_ok_and(|hidden| hidden != 0);
    if hidden || id.starts_with(AGGREGATE_UID_PREFIX) {
        return Ok(None);
    }
    let name = get_string(device, address(kAudioObjectPropertyName, global))?;
    let available = get_property::<u32>(
        device,
        address(kAudioDevicePropertyDeviceIsAlive, global),
        "read device state",
    )? != 0;
    let mut endpoints = Vec::with_capacity(2);
    for (scope, flow) in [
        (kAudioObjectPropertyScopeOutput, EndpointFlow::Render),
        (kAudioObjectPropertyScopeInput, EndpointFlow::Capture),
    ] {
        if channel_count(device, scope)? > 0 {
            endpoints.push(Endpoint {
                id: id.clone(),
                name: name.clone(),
                flow,
                available,
            });
        }
    }
    Ok(Some(endpoints))
}

fn device_for_uid(uid: &str) -> Result<AudioObjectID, AudioError> {
    let cf_uid = CFString::from_str(uid);
    let uid_ref: *const CFString = &*cf_uid;
    let address = address(
        kAudioHardwarePropertyTranslateUIDToDevice,
        kAudioObjectPropertyScopeGlobal,
    );
    let mut device = kAudioObjectUnknown;
    let mut size = size_of::<AudioObjectID>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&address),
            size_of::<*const CFString>() as u32,
            ptr::from_ref(&uid_ref).cast(),
            NonNull::from(&mut size),
            NonNull::from(&mut device).cast(),
        )
    };
    check(status, "look up audio device")?;
    if device == kAudioObjectUnknown {
        return Err(AudioError::EndpointNotFound(uid.into()));
    }
    Ok(device)
}

fn nominal_sample_rate(device: AudioObjectID) -> Result<f64, AudioError> {
    get_property(
        device,
        address(
            kAudioDevicePropertyNominalSampleRate,
            kAudioObjectPropertyScopeGlobal,
        ),
        "read device sample rate",
    )
}

fn channel_count(
    device: AudioObjectID,
    scope: AudioObjectPropertyScope,
) -> Result<usize, AudioError> {
    // AudioBufferList is variable-length; u64 storage keeps it aligned.
    let storage: Vec<u64> = get_property_array(
        device,
        address(kAudioDevicePropertyStreamConfiguration, scope),
        "read device channel layout",
    )?;
    if storage.is_empty() {
        return Ok(0);
    }
    let list = unsafe { &*storage.as_ptr().cast::<AudioBufferList>() };
    Ok(buffers(list)
        .iter()
        .map(|buffer| buffer.mNumberChannels as usize)
        .sum())
}

/// IOProcs receive each stream's virtual format, which is 32-bit float on
/// every standard macOS device. Anything else is rejected up front.
fn ensure_float_streams(
    device: AudioObjectID,
    scope: AudioObjectPropertyScope,
) -> Result<(), AudioError> {
    let streams: Vec<AudioObjectID> = get_property_array(
        device,
        address(kAudioDevicePropertyStreams, scope),
        "list device streams",
    )?;
    for stream in streams {
        let format: AudioStreamBasicDescription = get_property(
            stream,
            address(
                kAudioStreamPropertyVirtualFormat,
                kAudioObjectPropertyScopeGlobal,
            ),
            "read stream format",
        )?;
        if format.mFormatID != kAudioFormatLinearPCM
            || format.mFormatFlags & kAudioFormatFlagIsFloat == 0
            || format.mBitsPerChannel != 32
        {
            return Err(AudioError::UnsupportedFormat(format!(
                "stream {stream} is not 32-bit float PCM"
            )));
        }
    }
    Ok(())
}

fn address(
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}

fn get_property<T: Copy>(
    object: AudioObjectID,
    address: AudioObjectPropertyAddress,
    stage: &str,
) -> Result<T, AudioError> {
    let mut value = MaybeUninit::<T>::uninit();
    let mut size = size_of::<T>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::new_unchecked(value.as_mut_ptr().cast()),
        )
    };
    check(status, stage)?;
    if size as usize != size_of::<T>() {
        return Err(AudioError::Backend(format!(
            "{stage}: unexpected property size {size}"
        )));
    }
    Ok(unsafe { value.assume_init() })
}

fn get_property_array<T: Copy>(
    object: AudioObjectID,
    address: AudioObjectPropertyAddress,
    stage: &str,
) -> Result<Vec<T>, AudioError> {
    let mut size = 0_u32;
    check(
        unsafe {
            AudioObjectGetPropertyDataSize(
                object,
                NonNull::from(&address),
                0,
                ptr::null(),
                NonNull::from(&mut size),
            )
        },
        stage,
    )?;
    let mut values = Vec::<T>::with_capacity((size as usize).div_ceil(size_of::<T>()));
    if values.capacity() == 0 {
        return Ok(values);
    }
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::new_unchecked(values.as_mut_ptr().cast()),
        )
    };
    check(status, stage)?;
    unsafe { values.set_len((size as usize).div_ceil(size_of::<T>())) };
    Ok(values)
}

fn get_string(
    object: AudioObjectID,
    address: AudioObjectPropertyAddress,
) -> Result<String, AudioError> {
    let value: *const CFString = get_property(object, address, "read device string")?;
    let value = NonNull::new(value.cast_mut())
        .ok_or_else(|| AudioError::Backend("device string property was empty".into()))?;
    // Core Audio returns string properties retained; the caller releases them.
    Ok(unsafe { CFRetained::from_raw(value) }.to_string())
}

fn buffers(list: &AudioBufferList) -> &[AudioBuffer] {
    unsafe { std::slice::from_raw_parts(list.mBuffers.as_ptr(), list.mNumberBuffers as usize) }
}

fn buffers_mut(list: &mut AudioBufferList) -> &mut [AudioBuffer] {
    unsafe {
        std::slice::from_raw_parts_mut(list.mBuffers.as_mut_ptr(), list.mNumberBuffers as usize)
    }
}

fn buffer_samples(buffer: &AudioBuffer) -> &[f32] {
    if buffer.mData.is_null() {
        return &[];
    }
    unsafe {
        std::slice::from_raw_parts(
            buffer.mData.cast::<f32>(),
            buffer.mDataByteSize as usize / size_of::<f32>(),
        )
    }
}

fn buffer_samples_mut(buffer: &mut AudioBuffer) -> &mut [f32] {
    if buffer.mData.is_null() {
        return &mut [];
    }
    unsafe {
        std::slice::from_raw_parts_mut(
            buffer.mData.cast::<f32>(),
            buffer.mDataByteSize as usize / size_of::<f32>(),
        )
    }
}

fn frame_count(buffers: &[AudioBuffer]) -> usize {
    buffers
        .iter()
        .find(|buffer| buffer.mNumberChannels > 0)
        .map_or(0, |buffer| {
            buffer.mDataByteSize as usize / (size_of::<f32>() * buffer.mNumberChannels as usize)
        })
}

/// Reads one sample by channel index across all buffers in the list.
fn sample_at(buffers: &[AudioBuffer], channel: usize, frame: usize) -> f32 {
    let mut first_channel = 0;
    for buffer in buffers {
        let channels = buffer.mNumberChannels as usize;
        if channel < first_channel + channels {
            return buffer_samples(buffer)
                .get(frame * channels + channel - first_channel)
                .copied()
                .unwrap_or(0.0);
        }
        first_channel += channels;
    }
    0.0
}

fn check(status: i32, stage: &str) -> Result<(), AudioError> {
    if status == 0 {
        return Ok(());
    }
    // Core Audio errors are usually four-character codes such as '!dev'.
    let code = status.to_be_bytes();
    let detail = if code
        .iter()
        .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
    {
        format!("'{}'", String::from_utf8_lossy(&code))
    } else {
        status.to_string()
    };
    Err(AudioError::Backend(format!("{stage}: OSStatus {detail}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_character_status_codes_are_readable() {
        let error = check(i32::from_be_bytes(*b"!dev"), "start input device").unwrap_err();
        assert_eq!(
            error.to_string(),
            "audio backend failed: start input device: OSStatus '!dev'"
        );
        let error = check(-50, "read stream format").unwrap_err();
        assert!(error.to_string().ends_with("OSStatus -50"));
    }

    #[test]
    fn samples_are_addressed_across_non_interleaved_buffers() {
        let mut left = [0.1_f32, 0.2];
        let mut right = [0.3_f32, 0.4];
        let buffers = [
            AudioBuffer {
                mNumberChannels: 1,
                mDataByteSize: 8,
                mData: left.as_mut_ptr().cast(),
            },
            AudioBuffer {
                mNumberChannels: 1,
                mDataByteSize: 8,
                mData: right.as_mut_ptr().cast(),
            },
        ];
        assert_eq!(frame_count(&buffers), 2);
        assert_eq!(sample_at(&buffers, 1, 1), 0.4);
        assert_eq!(sample_at(&buffers, 2, 0), 0.0);
    }
}

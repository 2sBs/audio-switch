use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rtrb::{Consumer, Producer};
use wasapi::{
    Device, DeviceEnumerator, DeviceState, Direction, SampleType, StreamMode, WaveFormat,
    initialize_mta,
};

use super::microphone_activity::{ActivityMonitor, ActivityWake};
use crate::state::ActiveMic;

use super::bridge::{
    AdaptiveReader, AtomicDiagnostics, RouteDiagnostics, frames_for_ms, primed_ring,
};
use super::{AudioBackend, AudioError, CueKind, Endpoint, EndpointFlow, synthesize_cue};

const START_TIMEOUT: Duration = Duration::from_secs(8);
const EVENT_TIMEOUT_MS: u32 = 100;

#[derive(Debug)]
struct RouteHandle {
    stop: Arc<AtomicBool>,
    diagnostics: Arc<AtomicDiagnostics>,
    join: Option<JoinHandle<Result<(), AudioError>>>,
}

impl RouteHandle {
    fn stop(mut self) -> Result<(), AudioError> {
        self.stop.store(true, Ordering::Release);
        match self.join.take().expect("route thread is present").join() {
            Ok(result) => result,
            Err(_) => Err(AudioError::Backend("audio route thread panicked".into())),
        }
    }
}

pub struct WasapiBackend {
    sample_rate: u32,
    mirror_buffer_ms: u32,
    mic_buffer_ms: u32,
    mirror: Mutex<Option<RouteHandle>>,
    microphone: Mutex<Option<RouteHandle>>,
    activity: Mutex<Option<ActivityMonitor>>,
    activity_wake: ActivityWake,
}

impl WasapiBackend {
    pub fn new(sample_rate: u32, mirror_buffer_ms: u32, mic_buffer_ms: u32) -> Self {
        Self {
            sample_rate,
            mirror_buffer_ms,
            mic_buffer_ms,
            mirror: Mutex::new(None),
            microphone: Mutex::new(None),
            activity: Mutex::new(None),
            activity_wake: Arc::new(Mutex::new(None)),
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn set_activity_callback(&self, callback: impl Fn() + Send + Sync + 'static) {
        *self
            .activity_wake
            .lock()
            .expect("activity callback mutex poisoned") = Some(Arc::new(callback));
    }

    pub fn mirror_buffer_ms(&self) -> u32 {
        self.mirror_buffer_ms
    }

    pub fn mic_buffer_ms(&self) -> u32 {
        self.mic_buffer_ms
    }

    pub fn mirror_diagnostics(&self) -> Option<RouteDiagnostics> {
        self.mirror
            .lock()
            .expect("mirror mutex poisoned")
            .as_ref()
            .map(|route| route.diagnostics.snapshot())
    }

    pub fn microphone_diagnostics(&self) -> Option<RouteDiagnostics> {
        self.microphone
            .lock()
            .expect("microphone mutex poisoned")
            .as_ref()
            .map(|route| route.diagnostics.snapshot())
    }

    fn enumerate_direction(direction: Direction) -> Result<Vec<Endpoint>, AudioError> {
        let enumerator = DeviceEnumerator::new().map_err(audio_backend_error)?;
        let collection = enumerator
            .get_device_collection(&direction)
            .map_err(audio_backend_error)?;
        let flow = match direction {
            Direction::Render => EndpointFlow::Render,
            Direction::Capture => EndpointFlow::Capture,
        };
        // Fails when the direction has no default device, for example with no
        // speakers connected.
        let system_default = enumerator
            .get_default_device(&direction)
            .and_then(|device| device.get_id())
            .ok();

        collection
            .into_iter()
            .map(|device| {
                let device = device.map_err(audio_backend_error)?;
                let id = device.get_id().map_err(audio_backend_error)?;
                Ok(Endpoint {
                    system_default: system_default.as_ref() == Some(&id),
                    name: device.get_friendlyname().map_err(audio_backend_error)?,
                    flow,
                    available: matches!(
                        device.get_state().map_err(audio_backend_error)?,
                        DeviceState::Active
                    ),
                    // Endpoints on one audio adapter share its interface name.
                    // A virtual cable's input and output endpoints both belong
                    // to the cable driver's adapter.
                    device_id: device.get_interface_friendlyname().ok(),
                    id,
                })
            })
            .collect()
    }

    fn spawn_route(&self, specification: BridgeSpecification) -> Result<RouteHandle, AudioError> {
        let stop = Arc::new(AtomicBool::new(false));
        let diagnostics = Arc::new(AtomicDiagnostics::default());
        let thread_stop = Arc::clone(&stop);
        let thread_diagnostics = Arc::clone(&diagnostics);
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let join = thread::Builder::new()
            .name(specification.route_name.into())
            .spawn(move || run_bridge(specification, thread_stop, thread_diagnostics, started_tx))
            .map_err(audio_backend_error)?;

        match started_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(())) => Ok(RouteHandle {
                stop,
                diagnostics,
                join: Some(join),
            }),
            Ok(Err(message)) => {
                stop.store(true, Ordering::Release);
                let _ = join.join();
                Err(AudioError::Backend(
                    message
                        .strip_prefix("audio backend failed: ")
                        .unwrap_or(&message)
                        .to_owned(),
                ))
            }
            Err(error) => {
                stop.store(true, Ordering::Release);
                let _ = join.join();
                Err(AudioError::Backend(format!(
                    "audio route did not start within {} seconds: {error}",
                    START_TIMEOUT.as_secs()
                )))
            }
        }
    }

    fn replace_route(
        slot: &Mutex<Option<RouteHandle>>,
        route: RouteHandle,
    ) -> Result<(), AudioError> {
        let previous = slot.lock().expect("route mutex poisoned").replace(route);
        if let Some(previous) = previous {
            previous.stop()?;
        }
        Ok(())
    }

    fn stop_route(slot: &Mutex<Option<RouteHandle>>) -> Result<(), AudioError> {
        let route = slot.lock().expect("route mutex poisoned").take();
        if let Some(route) = route {
            route.stop()?;
        }
        Ok(())
    }
}

impl Drop for WasapiBackend {
    fn drop(&mut self) {
        if let Some(route) = self.mirror.get_mut().ok().and_then(Option::take) {
            let _ = route.stop();
        }
        if let Some(route) = self.microphone.get_mut().ok().and_then(Option::take) {
            let _ = route.stop();
        }
    }
}

impl AudioBackend for WasapiBackend {
    fn microphone_in_use(
        &self,
        sink_render_id: Option<&str>,
        capture_id: Option<&str>,
    ) -> Result<bool, AudioError> {
        let mut activity = self.activity.lock().expect("activity mutex poisoned");
        let Some(render_id) = sink_render_id else {
            *activity = None;
            return Ok(false);
        };
        if activity.as_ref().is_some_and(|monitor| {
            monitor.render_id != render_id || monitor.capture_override.as_deref() != capture_id
        }) {
            *activity = None;
        }
        if activity.is_none() {
            *activity = Some(ActivityMonitor::start(
                render_id,
                capture_id,
                Arc::clone(&self.activity_wake),
            )?);
        }
        let result = activity
            .as_ref()
            .expect("activity monitor is initialized")
            .in_use();
        if result.is_err() {
            *activity = None;
        }
        result
    }

    fn stop_activity_monitor(&self) {
        *self.activity.lock().expect("activity mutex poisoned") = None;
    }

    fn enumerate_endpoints(&self) -> Result<Vec<Endpoint>, AudioError> {
        with_com(|| {
            let mut endpoints = Self::enumerate_direction(Direction::Render)?;
            endpoints.extend(Self::enumerate_direction(Direction::Capture)?);
            endpoints.sort_by(|left, right| {
                left.flow
                    .cmp(&right.flow)
                    .then_with(|| left.name.cmp(&right.name))
            });
            Ok(endpoints)
        })
    }

    fn start_mirror(
        &self,
        source_render_id: &str,
        target_render_id: &str,
    ) -> Result<(), AudioError> {
        let route = self.spawn_route(BridgeSpecification {
            route_name: "audio-switch-mirror",
            source_id: source_render_id.into(),
            target_id: target_render_id.into(),
            source_kind: SourceKind::RenderLoopback,
            sample_rate: self.sample_rate,
            source_channels: 2,
            output_channels: 2,
            target_buffer_ms: self.mirror_buffer_ms,
        })?;
        Self::replace_route(&self.mirror, route)
    }

    fn stop_mirror(&self) -> Result<(), AudioError> {
        Self::stop_route(&self.mirror)
    }

    fn start_microphone(
        &self,
        source_capture_id: &str,
        _active: ActiveMic,
        sink_render_id: &str,
    ) -> Result<(), AudioError> {
        let route = self.spawn_route(BridgeSpecification {
            route_name: "audio-switch-microphone",
            source_id: source_capture_id.into(),
            target_id: sink_render_id.into(),
            source_kind: SourceKind::PhysicalCapture,
            sample_rate: self.sample_rate,
            source_channels: 2,
            output_channels: 1,
            target_buffer_ms: self.mic_buffer_ms,
        })?;
        Self::replace_route(&self.microphone, route)
    }

    fn stop_microphone(&self) -> Result<(), AudioError> {
        Self::stop_route(&self.microphone)
    }

    fn play_cue(
        &self,
        render_endpoint_id: &str,
        cue: CueKind,
        level_db: f32,
    ) -> Result<(), AudioError> {
        play_cue_once(render_endpoint_id, self.sample_rate, cue, level_db)
    }
}

#[derive(Debug, Clone, Copy)]
enum SourceKind {
    RenderLoopback,
    PhysicalCapture,
}

#[derive(Debug)]
struct BridgeSpecification {
    route_name: &'static str,
    source_id: String,
    target_id: String,
    source_kind: SourceKind,
    sample_rate: u32,
    source_channels: usize,
    output_channels: usize,
    target_buffer_ms: u32,
}

fn run_bridge(
    specification: BridgeSpecification,
    stop: Arc<AtomicBool>,
    diagnostics: Arc<AtomicDiagnostics>,
    started: mpsc::SyncSender<Result<(), String>>,
) -> Result<(), AudioError> {
    let target_frames = frames_for_ms(specification.sample_rate, specification.target_buffer_ms);
    let (producer, consumer) = primed_ring(target_frames, specification.output_channels);

    let capture_stop = Arc::clone(&stop);
    let capture_diagnostics = Arc::clone(&diagnostics);
    let capture_source_id = specification.source_id.clone();
    let source_kind = specification.source_kind;
    let source_channels = specification.source_channels;
    let output_channels = specification.output_channels;
    let sample_rate = specification.sample_rate;
    let (capture_ready_tx, capture_ready_rx) = mpsc::sync_channel(1);
    let capture = thread::Builder::new()
        .name(format!("{}-capture", specification.route_name))
        .spawn(move || {
            let result = capture_loop(
                &capture_source_id,
                source_kind,
                sample_rate,
                source_channels,
                output_channels,
                producer,
                capture_stop,
                capture_diagnostics,
                capture_ready_tx.clone(),
            );
            if let Err(error) = &result {
                let _ = capture_ready_tx.send(Err(error.to_string()));
            }
            result
        })
        .map_err(audio_backend_error)?;

    let render_stop = Arc::clone(&stop);
    let render_diagnostics = Arc::clone(&diagnostics);
    let target_id = specification.target_id.clone();
    let (render_ready_tx, render_ready_rx) = mpsc::sync_channel(1);
    let render = thread::Builder::new()
        .name(format!("{}-render", specification.route_name))
        .spawn(move || {
            let result = render_loop(
                &target_id,
                sample_rate,
                output_channels,
                target_frames,
                consumer,
                render_stop,
                render_diagnostics,
                render_ready_tx.clone(),
            );
            if let Err(error) = &result {
                let _ = render_ready_tx.send(Err(error.to_string()));
            }
            result
        })
        .map_err(audio_backend_error)?;

    let capture_ready = capture_ready_rx.recv_timeout(START_TIMEOUT);
    let render_ready = render_ready_rx.recv_timeout(START_TIMEOUT);
    let startup = match (capture_ready, render_ready) {
        (Ok(Ok(())), Ok(Ok(()))) => Ok(()),
        (Ok(Err(error)), _) | (_, Ok(Err(error))) => Err(error),
        (Err(error), _) | (_, Err(error)) => {
            Err(format!("audio worker startup timed out: {error}"))
        }
    };
    let startup_ok = startup.is_ok();
    let _ = started.send(startup);
    if !startup_ok {
        stop.store(true, Ordering::Release);
    }

    while !stop.load(Ordering::Acquire) && !capture.is_finished() && !render.is_finished() {
        thread::sleep(Duration::from_millis(20));
    }
    stop.store(true, Ordering::Release);

    let capture_result = capture
        .join()
        .map_err(|_| AudioError::Backend("capture thread panicked".into()))?;
    let render_result = render
        .join()
        .map_err(|_| AudioError::Backend("render thread panicked".into()))?;
    capture_result?;
    render_result?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn capture_loop(
    endpoint_id: &str,
    source_kind: SourceKind,
    sample_rate: u32,
    source_channels: usize,
    output_channels: usize,
    mut producer: Producer<f32>,
    stop: Arc<AtomicBool>,
    diagnostics: Arc<AtomicDiagnostics>,
    ready: mpsc::SyncSender<Result<(), String>>,
) -> Result<(), AudioError> {
    with_com(|| {
        let enumerator = DeviceEnumerator::new()
            .map_err(|error| stage_error("create capture device enumerator", error))?;
        let direction = match source_kind {
            SourceKind::RenderLoopback => Direction::Render,
            SourceKind::PhysicalCapture => Direction::Capture,
        };
        let device = find_active_device(&enumerator, direction, endpoint_id)?;
        let mut client = device
            .get_iaudioclient()
            .map_err(|error| stage_error("activate capture audio client", error))?;
        let format = WaveFormat::new(
            32,
            32,
            &SampleType::Float,
            sample_rate as usize,
            source_channels,
            None,
        );
        let (_, minimum_period) = client
            .get_device_period()
            .map_err(|error| stage_error("query capture device period", error))?;
        client
            .initialize_client(
                &format,
                &Direction::Capture,
                &StreamMode::EventsShared {
                    autoconvert: true,
                    buffer_duration_hns: minimum_period,
                },
            )
            .map_err(|error| stage_error("initialize shared capture stream", error))?;
        let event = client
            .set_get_eventhandle()
            .map_err(|error| stage_error("create capture event handle", error))?;
        let buffer_frames = client
            .get_buffer_size()
            .map_err(|error| stage_error("query capture buffer size", error))?
            as usize;
        let capture = client
            .get_audiocaptureclient()
            .map_err(|error| stage_error("activate capture buffer client", error))?;
        let bytes_per_frame = source_channels * size_of::<f32>();
        let mut bytes = vec![0_u8; buffer_frames * bytes_per_frame];
        client
            .start_stream()
            .map_err(|error| stage_error("start capture stream", error))?;
        let _guard = StreamGuard(&client);
        ready.send(Ok(())).ok();

        while !stop.load(Ordering::Acquire) {
            if event.wait_for_event(EVENT_TIMEOUT_MS).is_err() {
                continue;
            }
            loop {
                let packet_frames = capture
                    .get_next_packet_size()
                    .map_err(audio_backend_error)?
                    .unwrap_or(0) as usize;
                if packet_frames == 0 {
                    break;
                }
                if bytes.len() < packet_frames * bytes_per_frame {
                    return Err(AudioError::Backend(format!(
                        "capture packet exceeded the preallocated WASAPI buffer: {packet_frames} frames"
                    )));
                }
                let (frames_read, info) = capture
                    .read_from_device(&mut bytes)
                    .map_err(audio_backend_error)?;
                let frames_read = frames_read as usize;
                diagnostics
                    .captured_frames
                    .fetch_add(frames_read as u64, Ordering::Relaxed);
                if info.flags.data_discontinuity {
                    diagnostics.discontinuities.fetch_add(1, Ordering::Relaxed);
                }

                for frame in 0..frames_read {
                    if producer.slots() < output_channels {
                        diagnostics.overrun_frames.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    for output_channel in 0..output_channels {
                        let source_channel = match source_kind {
                            SourceKind::RenderLoopback => output_channel.min(source_channels - 1),
                            SourceKind::PhysicalCapture => 0,
                        };
                        let value = if info.flags.silent {
                            0.0
                        } else {
                            read_f32(
                                &bytes,
                                (frame * source_channels + source_channel) * size_of::<f32>(),
                            )
                        };
                        producer
                            .push(value)
                            .expect("capacity was checked for the complete frame");
                    }
                }
            }
        }
        Ok(())
    })
}

#[allow(clippy::too_many_arguments)]
fn render_loop(
    endpoint_id: &str,
    sample_rate: u32,
    channels: usize,
    target_frames: usize,
    consumer: Consumer<f32>,
    stop: Arc<AtomicBool>,
    diagnostics: Arc<AtomicDiagnostics>,
    ready: mpsc::SyncSender<Result<(), String>>,
) -> Result<(), AudioError> {
    with_com(|| {
        let enumerator = DeviceEnumerator::new()
            .map_err(|error| stage_error("create render device enumerator", error))?;
        let device = find_active_device(&enumerator, Direction::Render, endpoint_id)?;
        let mut client = device
            .get_iaudioclient()
            .map_err(|error| stage_error("activate render audio client", error))?;
        let format = WaveFormat::new(
            32,
            32,
            &SampleType::Float,
            sample_rate as usize,
            channels,
            None,
        );
        let (_, minimum_period) = client
            .get_device_period()
            .map_err(|error| stage_error("query render device period", error))?;
        client
            .initialize_client(
                &format,
                &Direction::Render,
                &StreamMode::EventsShared {
                    autoconvert: true,
                    buffer_duration_hns: minimum_period,
                },
            )
            .map_err(|error| stage_error("initialize shared render stream", error))?;
        let event = client
            .set_get_eventhandle()
            .map_err(|error| stage_error("create render event handle", error))?;
        let buffer_frames = client
            .get_buffer_size()
            .map_err(|error| stage_error("query render buffer size", error))?
            as usize;
        let render = client
            .get_audiorenderclient()
            .map_err(|error| stage_error("activate render buffer client", error))?;
        let mut samples = vec![0.0_f32; buffer_frames * channels];
        let mut bytes = vec![0_u8; buffer_frames * channels * size_of::<f32>()];
        let mut reader =
            AdaptiveReader::new(consumer, channels, target_frames, 1.0, diagnostics.clone());

        // Prime the endpoint with silence without draining the cross-device ring.
        // Shared-mode render buffers commonly request more than the low-latency mic
        // target on their first callback; consuming the ring here creates a one-time
        // underrun even when both clocks are stable afterwards.
        render
            .write_to_device(buffer_frames, &bytes, None)
            .map_err(|error| stage_error("prime render buffer", error))?;
        client
            .start_stream()
            .map_err(|error| stage_error("start render stream", error))?;
        let _guard = StreamGuard(&client);
        ready.send(Ok(())).ok();

        while !stop.load(Ordering::Acquire) {
            if event.wait_for_event(EVENT_TIMEOUT_MS).is_err() {
                continue;
            }
            let available = client
                .get_available_space_in_frames()
                .map_err(audio_backend_error)? as usize;
            if available == 0 {
                continue;
            }
            let sample_count = available * channels;
            reader.render(&mut samples[..sample_count]);
            f32_slice_to_bytes(&samples[..sample_count], &mut bytes[..sample_count * 4]);
            render
                .write_to_device(available, &bytes[..sample_count * 4], None)
                .map_err(audio_backend_error)?;
            diagnostics
                .rendered_frames
                .fetch_add(available as u64, Ordering::Relaxed);
        }
        Ok(())
    })
}

fn play_cue_once(
    endpoint_id: &str,
    sample_rate: u32,
    cue: CueKind,
    level_db: f32,
) -> Result<(), AudioError> {
    with_com(|| {
        let enumerator = DeviceEnumerator::new()
            .map_err(|error| stage_error("create cue device enumerator", error))?;
        let device = find_active_device(&enumerator, Direction::Render, endpoint_id)?;
        let mut client = device
            .get_iaudioclient()
            .map_err(|error| stage_error("activate cue audio client", error))?;
        let channels = 2;
        let format = WaveFormat::new(
            32,
            32,
            &SampleType::Float,
            sample_rate as usize,
            channels,
            None,
        );
        let (_, minimum_period) = client
            .get_device_period()
            .map_err(|error| stage_error("query cue device period", error))?;
        client
            .initialize_client(
                &format,
                &Direction::Render,
                &StreamMode::EventsShared {
                    autoconvert: true,
                    buffer_duration_hns: minimum_period,
                },
            )
            .map_err(|error| stage_error("initialize cue stream", error))?;
        let event = client
            .set_get_eventhandle()
            .map_err(|error| stage_error("create cue event handle", error))?;
        let buffer_frames = client.get_buffer_size().map_err(audio_backend_error)? as usize;
        let render = client
            .get_audiorenderclient()
            .map_err(audio_backend_error)?;
        let mono = synthesize_cue(cue, sample_rate, level_db);
        let mut stereo = Vec::with_capacity(mono.len() * channels);
        for sample in mono {
            stereo.extend_from_slice(&[sample, sample]);
        }
        let mut byte_buffer = vec![0_u8; buffer_frames * channels * size_of::<f32>()];
        let mut frame_offset = 0;

        let initial_frames = buffer_frames.min(stereo.len() / channels);
        f32_slice_to_bytes(
            &stereo[..initial_frames * channels],
            &mut byte_buffer[..initial_frames * channels * 4],
        );
        render
            .write_to_device(
                initial_frames,
                &byte_buffer[..initial_frames * channels * 4],
                None,
            )
            .map_err(|error| stage_error("prime cue stream", error))?;
        frame_offset += initial_frames;
        client
            .start_stream()
            .map_err(|error| stage_error("start cue stream", error))?;
        let _guard = StreamGuard(&client);

        while frame_offset < stereo.len() / channels {
            if event.wait_for_event(EVENT_TIMEOUT_MS).is_err() {
                continue;
            }
            let available = client
                .get_available_space_in_frames()
                .map_err(audio_backend_error)? as usize;
            let remaining = stereo.len() / channels - frame_offset;
            let frames = available.min(remaining);
            let start = frame_offset * channels;
            let end = start + frames * channels;
            f32_slice_to_bytes(
                &stereo[start..end],
                &mut byte_buffer[..frames * channels * 4],
            );
            render
                .write_to_device(frames, &byte_buffer[..frames * channels * 4], None)
                .map_err(|error| stage_error("write cue frames", error))?;
            frame_offset += frames;
        }

        // Allow the final shared-mode buffer to reach the physical endpoint.
        thread::sleep(Duration::from_millis(30));
        Ok(())
    })
}

struct StreamGuard<'a>(&'a wasapi::AudioClient);

impl Drop for StreamGuard<'_> {
    fn drop(&mut self) {
        let _ = self.0.stop_stream();
    }
}

pub(super) fn with_com<T>(
    operation: impl FnOnce() -> Result<T, AudioError>,
) -> Result<T, AudioError> {
    initialize_mta().ok().map_err(audio_backend_error)?;
    let result = operation();
    wasapi::deinitialize();
    result
}

fn find_active_device(
    enumerator: &DeviceEnumerator,
    direction: Direction,
    endpoint_id: &str,
) -> Result<Device, AudioError> {
    let collection = enumerator
        .get_device_collection(&direction)
        .map_err(|error| stage_error("enumerate active audio endpoints", error))?;
    for device in &collection {
        let device = device.map_err(audio_backend_error)?;
        let candidate_id = device.get_id().map_err(audio_backend_error)?;
        if candidate_id.eq_ignore_ascii_case(endpoint_id) {
            return Ok(device);
        }
    }
    Err(AudioError::EndpointNotFound(endpoint_id.into()))
}

fn read_f32(bytes: &[u8], offset: usize) -> f32 {
    f32::from_ne_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
}

fn f32_slice_to_bytes(samples: &[f32], bytes: &mut [u8]) {
    debug_assert_eq!(bytes.len(), std::mem::size_of_val(samples));
    for (sample, output) in samples.iter().zip(bytes.chunks_exact_mut(4)) {
        output.copy_from_slice(&sample.to_ne_bytes());
    }
}

fn audio_backend_error(error: impl std::fmt::Display) -> AudioError {
    AudioError::Backend(error.to_string())
}

fn stage_error(stage: &str, error: impl std::fmt::Display) -> AudioError {
    let message = error.to_string();
    let detail = if message.contains("0x8889000A") {
        "the audio device is in exclusive use by another application"
    } else {
        &message
    };
    AudioError::Backend(format!("{stage}: {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_in_use_errors_explain_the_exclusive_lock() {
        let error = stage_error(
            "initialize shared render stream",
            "Windows returned an error: 0x8889000A",
        );
        assert_eq!(
            error.to_string(),
            "audio backend failed: initialize shared render stream: the audio device is in exclusive use by another application"
        );
    }

    #[test]
    fn float_byte_conversion_round_trips() {
        let values = [-1.0_f32, 0.0, 0.5, 1.0];
        let mut bytes = [0_u8; 16];
        f32_slice_to_bytes(&values, &mut bytes);
        for (index, expected) in values.into_iter().enumerate() {
            assert_eq!(read_f32(&bytes, index * 4), expected);
        }
    }
}

use std::mem::size_of;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, anyhow};
use audio_switch_core::audio::{AudioBackend, WasapiBackend};
use audio_switch_core::ipc::{ControlCommand, ControlMessage};
use audio_switch_core::{ActiveMic, AppConfig, ConfigStore, RouteStatus, RoutingController};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT,
    POINT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, UnregisterHotKey, VK_F11,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CW_USEDEFAULT, CreateIconFromResourceEx, CreatePopupMenu, CreateWindowExW,
    DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetMessageW, GetSystemMetrics, HICON, LR_DEFAULTCOLOR, MB_ICONERROR, MB_ICONINFORMATION, MB_OK,
    MB_SETFOREGROUND, MB_TOPMOST, MF_CHECKED, MF_SEPARATOR, MF_STRING, MSG, MessageBoxW,
    PostQuitMessage, RegisterClassW, SM_CXSMICON, SetForegroundWindow, SetTimer, TPM_BOTTOMALIGN,
    TPM_LEFTALIGN, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_APP, WM_COMMAND, WM_DESTROY, WM_HOTKEY, WM_LBUTTONUP, WM_RBUTTONUP, WM_TIMER,
    WNDCLASSW,
};
use windows::core::{PCWSTR, w};

use crate::{settings, setup, ui};
use audio_switch::local_control;

const TRAY_ICON_ID: u32 = 1;
const HOTKEY_ID: i32 = 1;
const REFRESH_TIMER_ID: usize = 1;
const WM_TRAY: u32 = WM_APP + 1;

const MENU_PRIMARY: usize = 1001;
const MENU_SECONDARY: usize = 1002;
const MENU_SETTINGS: usize = 1003;
const MENU_STATUS: usize = 1004;
const MENU_EXIT: usize = 1099;

const PRIMARY_ICON: &[u8] = include_bytes!("../assets/tray/primary.ico");
const SECONDARY_ICON: &[u8] = include_bytes!("../assets/tray/secondary.ico");
const WARNING_ICON: &[u8] = include_bytes!("../assets/tray/warning.ico");

static CONTEXT: OnceLock<Mutex<TrayContext>> = OnceLock::new();

struct TrayContext {
    controller: RoutingController<WasapiBackend>,
    setup_active: bool,
}

pub(crate) struct SingleInstanceGuard(HANDLE);

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub(crate) fn acquire_single_instance() -> Result<Option<SingleInstanceGuard>> {
    let instance = try_acquire_named_mutex(w!("Local\\AudioSwitch.Tray.v1"))?;
    if instance.is_none() {
        unsafe {
            MessageBoxW(
                None,
                w!(
                    "Audio Switch is already running in the notification area. Only one tray-app instance can run at a time. Close the existing tray app before starting another copy."
                ),
                w!("Audio Switch is already running"),
                MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
            );
        }
    }
    Ok(instance)
}

fn try_acquire_named_mutex(name: PCWSTR) -> Result<Option<SingleInstanceGuard>> {
    let mutex =
        unsafe { CreateMutexW(None, false, name) }.context("creating the single-instance mutex")?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = CloseHandle(mutex);
        }
        Ok(None)
    } else {
        Ok(Some(SingleInstanceGuard(mutex)))
    }
}

pub fn run(
    backend: WasapiBackend,
    store: ConfigStore,
    config: AppConfig,
    show_settings: bool,
    _instance: SingleInstanceGuard,
) -> Result<()> {
    let hotkey_enabled = config.hotkey.enabled;
    let mut controller = RoutingController::new(backend, store, config);
    controller.start().context("starting audio routes")?;
    CONTEXT
        .set(Mutex::new(TrayContext {
            controller,
            setup_active: false,
        }))
        .map_err(|_| anyhow!("tray context was already initialized"))?;
    local_control::start_server(handle_control_command)
        .context("starting the local control server")?;

    let module = unsafe { GetModuleHandleW(None) }.context("getting application module")?;
    let instance = HINSTANCE(module.0);
    let class = WNDCLASSW {
        hInstance: instance,
        hIcon: ui::app_icon(instance),
        lpszClassName: w!("AudioSwitch.TrayWindow.v1"),
        lpfnWndProc: Some(window_proc),
        ..Default::default()
    };
    if unsafe { RegisterClassW(&class) } == 0 {
        return Err(anyhow!("registering tray window class failed"));
    }

    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class.lpszClassName,
            w!("Audio Switch"),
            WINDOW_STYLE::default(),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        )
    }
    .context("creating tray message window")?;

    add_or_update_icon(window, true).context("adding tray icon")?;
    if hotkey_enabled {
        unsafe {
            RegisterHotKey(
                Some(window),
                HOTKEY_ID,
                MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT,
                VK_F11.0 as u32,
            )
        }
        .context("registering Ctrl+Shift+F11")?;
    }
    if unsafe { SetTimer(Some(window), REFRESH_TIMER_ID, 1_000, None) } == 0 {
        return Err(anyhow!("creating endpoint refresh timer failed"));
    }
    if show_settings {
        open_settings(window);
    }

    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    Ok(())
}

fn handle_control_command(command: ControlCommand) -> ControlMessage {
    let request_id = match &command {
        ControlCommand::GetState { request_id }
        | ControlCommand::Subscribe { request_id }
        | ControlCommand::ToggleMic { request_id }
        | ControlCommand::SetActiveMic { request_id, .. } => request_id.clone(),
    };
    let Some(context) = CONTEXT.get() else {
        return ControlMessage::Error {
            request_id,
            message: "tray controller is not ready".into(),
        };
    };
    let mut context = context.lock().expect("tray mutex poisoned");
    if context.setup_active && control_command_changes_active_mic(&command) {
        return ControlMessage::Error {
            request_id,
            message: "setup is in progress".into(),
        };
    }
    let result = match command {
        ControlCommand::GetState { .. } | ControlCommand::Subscribe { .. } => Ok(()),
        ControlCommand::ToggleMic { .. } => context.controller.toggle_mic(),
        ControlCommand::SetActiveMic { active_mic, .. } => {
            context.controller.select_mic(active_mic)
        }
    };
    match result {
        Ok(()) => ControlMessage::State {
            request_id,
            state: context.controller.state().clone(),
            labels: context.controller.config().labels.clone(),
        },
        Err(error) => ControlMessage::Error {
            request_id,
            message: error.to_string(),
        },
    }
}

fn control_command_changes_active_mic(command: &ControlCommand) -> bool {
    matches!(
        command,
        ControlCommand::ToggleMic { .. } | ControlCommand::SetActiveMic { .. }
    )
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_HOTKEY if !setup_is_active() => {
            toggle(window);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == REFRESH_TIMER_ID && !setup_is_active() => {
            refresh(window);
            LRESULT(0)
        }
        WM_COMMAND => {
            let command = wparam.0 & 0xffff;
            if setup_is_active() && command != MENU_EXIT {
                return LRESULT(0);
            }
            match command {
                MENU_PRIMARY => select(window, ActiveMic::Primary),
                MENU_SECONDARY => select(window, ActiveMic::Secondary),
                MENU_SETTINGS => open_settings(window),
                MENU_STATUS => show_status(window),
                MENU_EXIT => unsafe {
                    let _ = DestroyWindow(window);
                },
                _ => {}
            }
            LRESULT(0)
        }
        WM_TRAY => {
            if setup_is_active() {
                return LRESULT(0);
            }
            match lparam.0 as u32 {
                WM_LBUTTONUP => toggle(window),
                WM_RBUTTONUP => show_menu(window),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(context) = CONTEXT.get() {
                context
                    .lock()
                    .expect("tray mutex poisoned")
                    .controller
                    .stop();
            }
            let data = icon_data(window, HICON::default(), "Audio Switch");
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
                let _ = UnregisterHotKey(Some(window), HOTKEY_ID);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

fn toggle(window: HWND) {
    with_controller(|controller| controller.toggle_mic());
    let _ = add_or_update_icon(window, false);
}

fn select(window: HWND, active: ActiveMic) {
    with_controller(|controller| controller.select_mic(active));
    let _ = add_or_update_icon(window, false);
}

fn refresh(window: HWND) {
    with_controller(|controller| controller.refresh_endpoints());
    let _ = add_or_update_icon(window, false);
}

fn with_controller(
    action: impl FnOnce(
        &mut RoutingController<WasapiBackend>,
    ) -> Result<(), audio_switch_core::ControllerError>,
) {
    let Some(context) = CONTEXT.get() else {
        return;
    };
    if let Err(error) = action(&mut context.lock().expect("tray mutex poisoned").controller) {
        eprintln!("Audio Switch control action failed: {error}");
    }
}

fn open_settings(window: HWND) {
    let Some(context) = CONTEXT.get() else {
        return;
    };
    let context = context.lock().expect("tray mutex poisoned");
    let config = context.controller.config().clone();
    let endpoints = match context.controller.backend().enumerate_endpoints() {
        Ok(endpoints) => endpoints,
        Err(error) => {
            eprintln!("Audio Switch could not list settings endpoints: {error}");
            return;
        }
    };
    drop(context);
    if let Err(error) = settings::open(
        window,
        config,
        endpoints,
        apply_settings,
        reset_and_run_setup,
    ) {
        eprintln!("Audio Switch settings failed: {error:#}");
    }
}

fn apply_settings(config: AppConfig) -> Result<()> {
    let context = CONTEXT.get().context("tray controller is unavailable")?;
    context
        .lock()
        .expect("tray mutex poisoned")
        .controller
        .apply_config(config.clone())
        .context("applying audio settings")?;
    #[cfg(not(debug_assertions))]
    crate::autostart::reconcile(
        config.auto_start,
        config.auto_start_elevated,
        &std::env::current_exe().context("locating Audio Switch executable")?,
    )?;
    Ok(())
}

fn setup_is_active() -> bool {
    CONTEXT
        .get()
        .is_some_and(|context| context.lock().expect("tray mutex poisoned").setup_active)
}

fn reset_and_run_setup(window: HWND) -> Result<()> {
    let endpoints = crate::enumerate_endpoints().context("enumerating setup endpoints")?;
    let current_config = {
        let mut context = CONTEXT
            .get()
            .context("tray controller is unavailable")?
            .lock()
            .expect("tray mutex poisoned");
        context.setup_active = true;
        context.controller.config().clone()
    };

    let setup_result = setup::run(
        setup_defaults_preserving_hidden(&current_config),
        endpoints,
        crate::enumerate_endpoints,
    );
    match setup_result {
        Ok(Some(config)) => {
            let result = apply_settings(config);
            set_setup_active(false);
            result
        }
        Ok(None) => {
            set_setup_active(false);
            open_settings(window);
            Ok(())
        }
        Err(error) => {
            set_setup_active(false);
            Err(error)
        }
    }
}

fn set_setup_active(active: bool) {
    if let Some(context) = CONTEXT.get() {
        context.lock().expect("tray mutex poisoned").setup_active = active;
    }
}

fn setup_defaults_preserving_hidden(current: &AppConfig) -> AppConfig {
    let mut config = AppConfig {
        active_mic: current.active_mic,
        hotkey: current.hotkey.clone(),
        ..AppConfig::default()
    };
    config.audio.sample_rate = current.audio.sample_rate;
    config.audio.mirror_buffer_ms = current.audio.mirror_buffer_ms;
    config.audio.mic_buffer_ms = current.audio.mic_buffer_ms;
    config.audio.cue_level_db = current.audio.cue_level_db;
    config
}

fn show_status(window: HWND) {
    let Some(context) = CONTEXT.get() else {
        return;
    };
    let text = {
        let context = context.lock().expect("tray mutex poisoned");
        let state = context.controller.state();
        let labels = &context.controller.config().labels;
        let active_label = match state.active_mic {
            ActiveMic::Primary => &labels.primary,
            ActiveMic::Secondary => &labels.secondary,
        };
        format!(
            "Active microphone: {active_label}\r\nMicrophone route: {}\r\nPlayback mirror: {}",
            route_status_text(&state.mic_status),
            route_status_text(&state.mirror_status)
        )
    };
    let wide = wide_null(&text);
    unsafe {
        MessageBoxW(
            Some(window),
            windows::core::PCWSTR(wide.as_ptr()),
            w!("Audio Switch status"),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

fn route_status_text(status: &RouteStatus) -> String {
    match status {
        RouteStatus::Stopped => "Stopped".into(),
        RouteStatus::Starting => "Starting".into(),
        RouteStatus::Routing => "Routing".into(),
        RouteStatus::Waiting { reason } => format!("Waiting: {reason}"),
        RouteStatus::Error { message } => format!("Error: {message}"),
    }
}

fn show_menu(window: HWND) {
    let menu = match unsafe { CreatePopupMenu() } {
        Ok(menu) => menu,
        Err(_) => return,
    };
    let (active, primary_label, secondary_label) = CONTEXT
        .get()
        .map(|context| {
            let context = context.lock().expect("tray mutex poisoned");
            (
                context.controller.state().active_mic,
                context.controller.config().labels.primary.clone(),
                context.controller.config().labels.secondary.clone(),
            )
        })
        .unwrap_or((ActiveMic::Primary, "Primary".into(), "Secondary".into()));
    let primary_flags = MF_STRING
        | if active == ActiveMic::Primary {
            MF_CHECKED
        } else {
            Default::default()
        };
    let secondary_flags = MF_STRING
        | if active == ActiveMic::Secondary {
            MF_CHECKED
        } else {
            Default::default()
        };
    let primary_label = wide_null(&format!("{primary_label} microphone"));
    let secondary_label = wide_null(&format!("{secondary_label} microphone"));
    unsafe {
        AppendMenuW(
            menu,
            primary_flags,
            MENU_PRIMARY,
            windows::core::PCWSTR(primary_label.as_ptr()),
        )
        .ok();
        AppendMenuW(
            menu,
            secondary_flags,
            MENU_SECONDARY,
            windows::core::PCWSTR(secondary_label.as_ptr()),
        )
        .ok();
        AppendMenuW(menu, MF_SEPARATOR, 0, windows::core::PCWSTR::null()).ok();
        AppendMenuW(menu, MF_STRING, MENU_SETTINGS, w!("Settings...")).ok();
        AppendMenuW(menu, MF_STRING, MENU_STATUS, w!("Status")).ok();
        AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("Exit")).ok();
        let mut point = POINT::default();
        if GetCursorPos(&mut point).is_ok() {
            let _ = SetForegroundWindow(window);
            let _ = TrackPopupMenu(
                menu,
                TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                None,
                window,
                None,
            );
        }
        DestroyMenu(menu).ok();
    }
}

fn add_or_update_icon(window: HWND, add: bool) -> Result<()> {
    let (tooltip, icon_bytes) = CONTEXT
        .get()
        .map(|context| {
            let context = context.lock().expect("tray mutex poisoned");
            let state = context.controller.state();
            let warning = !matches!(state.mic_status, RouteStatus::Routing)
                || !matches!(state.mirror_status, RouteStatus::Routing);
            let labels = &context.controller.config().labels;
            let active_label = match state.active_mic {
                ActiveMic::Primary => &labels.primary,
                ActiveMic::Secondary => &labels.secondary,
            };
            let icon_bytes = if warning {
                WARNING_ICON
            } else {
                match state.active_mic {
                    ActiveMic::Primary => PRIMARY_ICON,
                    ActiveMic::Secondary => SECONDARY_ICON,
                }
            };
            (format!("Audio Switch - {active_label} mic"), icon_bytes)
        })
        .unwrap_or_else(|| ("Audio Switch".into(), WARNING_ICON));
    let icon = load_tray_icon(icon_bytes)?;
    let data = icon_data(window, icon, &tooltip);
    let result = unsafe { Shell_NotifyIconW(if add { NIM_ADD } else { NIM_MODIFY }, &data) };
    unsafe { DestroyIcon(icon) }.ok();
    if !result.as_bool() {
        return Err(anyhow!("Windows rejected the tray icon operation"));
    }
    Ok(())
}

fn load_tray_icon(ico: &[u8]) -> Result<HICON> {
    let desired_size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
    let image = ico_image_for_size(ico, desired_size as u32)
        .context("selecting the embedded tray icon size")?;
    unsafe {
        CreateIconFromResourceEx(
            image,
            true,
            0x0003_0000,
            desired_size,
            desired_size,
            LR_DEFAULTCOLOR,
        )
    }
    .context("loading the embedded tray icon")
}

fn ico_image_for_size(ico: &[u8], desired_size: u32) -> Option<&[u8]> {
    if ico.len() < 6 || ico[0..4] != [0, 0, 1, 0] {
        return None;
    }
    let image_count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
    if ico.len() < 6 + image_count * 16 {
        return None;
    }

    let mut best: Option<(u32, &[u8])> = None;
    for index in 0..image_count {
        let entry = 6 + index * 16;
        let width = if ico[entry] == 0 {
            256
        } else {
            ico[entry] as u32
        };
        let byte_count = u32::from_le_bytes(
            ico[entry + 8..entry + 12]
                .try_into()
                .expect("ICO entry byte count has a fixed width"),
        ) as usize;
        let image_offset = u32::from_le_bytes(
            ico[entry + 12..entry + 16]
                .try_into()
                .expect("ICO entry offset has a fixed width"),
        ) as usize;
        let image_end = image_offset.checked_add(byte_count)?;
        let image = ico.get(image_offset..image_end)?;
        let distance = width.abs_diff(desired_size);
        if best.is_none_or(|(best_distance, _)| distance < best_distance) {
            best = Some((distance, image));
        }
    }
    best.map(|(_, image)| image)
}

fn icon_data(window: HWND, icon: HICON, tooltip: &str) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: TRAY_ICON_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: icon,
        ..Default::default()
    };
    copy_wide(tooltip, &mut data.szTip);
    data
}

fn copy_wide(value: &str, destination: &mut [u16]) {
    destination.fill(0);
    let maximum_length = destination.len().saturating_sub(1);
    for (slot, value) in destination
        .iter_mut()
        .take(maximum_length)
        .zip(value.encode_utf16())
    {
        *slot = value;
    }
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use windows::Win32::UI::WindowsAndMessaging::DestroyIcon;

    use super::*;

    static NEXT_MUTEX_ID: AtomicUsize = AtomicUsize::new(1);

    #[test]
    fn status_text_is_human_readable() {
        assert_eq!(route_status_text(&RouteStatus::Routing), "Routing");
        assert_eq!(
            route_status_text(&RouteStatus::Error {
                message: "device is in use".into()
            }),
            "Error: device is in use"
        );
    }

    #[test]
    fn reset_setup_preserves_preferences_the_wizard_does_not_show() {
        let mut current = AppConfig {
            active_mic: ActiveMic::Secondary,
            ..AppConfig::default()
        };
        current.hotkey.enabled = false;
        current.hotkey.key = "F10".into();
        current.audio.sample_rate = 44_100;
        current.audio.mirror_buffer_ms = 47;
        current.audio.mic_buffer_ms = 13;
        current.audio.cue_level_db = -12.0;
        current.audio.cue_enabled = false;
        current.auto_start = false;

        let reset = setup_defaults_preserving_hidden(&current);

        assert_eq!(reset.active_mic, ActiveMic::Secondary);
        assert_eq!(reset.hotkey, current.hotkey);
        assert_eq!(reset.audio.sample_rate, 44_100);
        assert_eq!(reset.audio.mirror_buffer_ms, 47);
        assert_eq!(reset.audio.mic_buffer_ms, 13);
        assert_eq!(reset.audio.cue_level_db, -12.0);
        assert!(reset.audio.cue_enabled);
        assert!(reset.auto_start);
    }

    #[test]
    fn setup_blocks_only_mutating_control_commands() {
        assert!(control_command_changes_active_mic(
            &ControlCommand::ToggleMic { request_id: None }
        ));
        assert!(control_command_changes_active_mic(
            &ControlCommand::SetActiveMic {
                request_id: None,
                active_mic: ActiveMic::Primary,
            }
        ));
        assert!(!control_command_changes_active_mic(
            &ControlCommand::GetState { request_id: None }
        ));
        assert!(!control_command_changes_active_mic(
            &ControlCommand::Subscribe { request_id: None }
        ));
    }

    #[test]
    fn named_mutex_allows_only_one_live_tray_instance() {
        let id = NEXT_MUTEX_ID.fetch_add(1, Ordering::Relaxed);
        let name = wide_null(&format!(
            "Local\\AudioSwitch.Tray.Test.{}.{}",
            std::process::id(),
            id
        ));
        let name = PCWSTR(name.as_ptr());

        let first = try_acquire_named_mutex(name).unwrap();
        assert!(first.is_some());
        assert!(try_acquire_named_mutex(name).unwrap().is_none());

        drop(first);
        assert!(try_acquire_named_mutex(name).unwrap().is_some());
    }

    #[test]
    fn embedded_tray_icons_contain_common_windows_sizes() {
        for icon in [PRIMARY_ICON, SECONDARY_ICON, WARNING_ICON] {
            assert!(ico_image_for_size(icon, 16).is_some());
            assert!(ico_image_for_size(icon, 24).is_some());
            assert!(ico_image_for_size(icon, 32).is_some());
        }
    }

    #[test]
    fn windows_can_load_each_embedded_tray_icon() {
        for icon_bytes in [PRIMARY_ICON, SECONDARY_ICON, WARNING_ICON] {
            let icon = load_tray_icon(icon_bytes).expect("embedded icon should load");
            unsafe { DestroyIcon(icon) }.expect("loaded icon should be released");
        }
    }
}

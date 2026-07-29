use std::mem::size_of;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, anyhow};
use audio_switch_core::audio::{AudioBackend, WasapiBackend};
use audio_switch_core::ipc::{ControlCommand, ControlMessage};
use audio_switch_core::{ActiveMic, AppConfig, ConfigStore, RouteStatus, RoutingController};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT,
    WPARAM,
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
    AppendMenuW, CW_USEDEFAULT, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, HICON, IDI_APPLICATION,
    IDI_WARNING, LoadIconW, MB_ICONINFORMATION, MB_OK, MF_CHECKED, MF_SEPARATOR, MF_STRING, MSG,
    MessageBoxW, PostQuitMessage, RegisterClassW, SetForegroundWindow, SetTimer, TPM_BOTTOMALIGN,
    TPM_LEFTALIGN, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_APP, WM_COMMAND, WM_DESTROY, WM_HOTKEY, WM_LBUTTONUP, WM_RBUTTONUP, WM_TIMER,
    WNDCLASSW,
};
use windows::core::w;

use crate::{local_control, settings};

const TRAY_ICON_ID: u32 = 1;
const HOTKEY_ID: i32 = 1;
const REFRESH_TIMER_ID: usize = 1;
const WM_TRAY: u32 = WM_APP + 1;

const MENU_PRIMARY: usize = 1001;
const MENU_SECONDARY: usize = 1002;
const MENU_SETTINGS: usize = 1003;
const MENU_STATUS: usize = 1004;
const MENU_EXIT: usize = 1099;

static CONTEXT: OnceLock<Mutex<TrayContext>> = OnceLock::new();

struct TrayContext {
    controller: RoutingController<WasapiBackend>,
}

pub fn run(
    backend: WasapiBackend,
    store: ConfigStore,
    config: AppConfig,
    show_settings: bool,
) -> Result<()> {
    let hotkey_enabled = config.hotkey.enabled;
    let mutex = unsafe { CreateMutexW(None, true, w!("Local\\AudioSwitch.Tray.v1")) }
        .context("creating the single-instance mutex")?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe { CloseHandle(mutex) }.ok();
        return Ok(());
    }

    let mut controller = RoutingController::new(backend, store, config);
    controller.start().context("starting audio routes")?;
    CONTEXT
        .set(Mutex::new(TrayContext { controller }))
        .map_err(|_| anyhow!("tray context was already initialized"))?;
    local_control::start_server(handle_control_command);

    let module = unsafe { GetModuleHandleW(None) }.context("getting application module")?;
    let instance = HINSTANCE(module.0);
    let class = WNDCLASSW {
        hInstance: instance,
        lpszClassName: w!("AudioSwitch.TrayWindow.v1"),
        lpfnWndProc: Some(window_proc),
        ..Default::default()
    };
    if unsafe { RegisterClassW(&class) } == 0 {
        unsafe { CloseHandle(mutex) }.ok();
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

    unsafe { CloseHandle(mutex) }.ok();
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

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_HOTKEY => {
            toggle(window);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == REFRESH_TIMER_ID => {
            refresh(window);
            LRESULT(0)
        }
        WM_COMMAND => {
            let command = wparam.0 & 0xffff;
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
    if let Err(error) = settings::open(window, config, endpoints, apply_settings) {
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

fn show_status(window: HWND) {
    let Some(context) = CONTEXT.get() else {
        return;
    };
    let context = context.lock().expect("tray mutex poisoned");
    let state = context.controller.state();
    let labels = &context.controller.config().labels;
    let active_label = match state.active_mic {
        ActiveMic::Primary => &labels.primary,
        ActiveMic::Secondary => &labels.secondary,
    };
    let text = format!(
        "Active microphone: {active_label}\r\nMicrophone route: {:?}\r\nPlayback mirror: {:?}",
        state.mic_status, state.mirror_status
    );
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
    let (tooltip, warning) = CONTEXT
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
            (format!("Audio Switch - {active_label} mic"), warning)
        })
        .unwrap_or_else(|| ("Audio Switch".into(), true));
    let icon = unsafe {
        LoadIconW(
            None,
            if warning {
                IDI_WARNING
            } else {
                IDI_APPLICATION
            },
        )
    }
    .context("loading tray icon")?;
    let data = icon_data(window, icon, &tooltip);
    let result = unsafe { Shell_NotifyIconW(if add { NIM_ADD } else { NIM_MODIFY }, &data) };
    if !result.as_bool() {
        return Err(anyhow!("Windows rejected the tray icon operation"));
    }
    Ok(())
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

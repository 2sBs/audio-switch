use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, anyhow};
use audio_switch_core::AppConfig;
use audio_switch_core::audio::{Endpoint, EndpointFlow};
use windows::Win32::Foundation::{GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL,
    CBS_DROPDOWNLIST, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow,
    ES_AUTOHSCROLL, GetWindowTextLengthW, GetWindowTextW, HMENU, MB_ICONERROR, MB_OK, MessageBoxW,
    RegisterClassW, SW_SHOW, SendMessageW, SetForegroundWindow, ShowWindow, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_DESTROY, WNDCLASSW, WS_BORDER, WS_CAPTION, WS_CHILD,
    WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};
use windows::core::{PCWSTR, w};

const ID_PRIMARY_NAME: usize = 100;
const ID_SECONDARY_NAME: usize = 101;
const ID_PRIMARY_RENDER: usize = 200;
const ID_SECONDARY_RENDER: usize = 201;
const ID_PRIMARY_CAPTURE: usize = 202;
const ID_SECONDARY_CAPTURE: usize = 203;
const ID_VIRTUAL_RENDER: usize = 204;
const ID_CUES: usize = 300;
const ID_AUTOSTART: usize = 301;
const ID_AUTOSTART_ELEVATED: usize = 302;
const ID_SAVE: usize = 400;
const ID_CANCEL: usize = 401;

type SaveCallback = fn(AppConfig) -> Result<()>;

struct ComboBinding {
    window: HWND,
    values: Vec<Option<String>>,
}

struct SettingsContext {
    window: HWND,
    primary_name: HWND,
    secondary_name: HWND,
    primary_render: ComboBinding,
    secondary_render: ComboBinding,
    primary_capture: ComboBinding,
    secondary_capture: ComboBinding,
    virtual_render: ComboBinding,
    cue_enabled: HWND,
    auto_start: HWND,
    auto_start_elevated: HWND,
    config: AppConfig,
    save: SaveCallback,
}

// The context is created, read, and destroyed only by the tray's Windows message
// thread. The mutex exists to prevent re-entrant access while using static storage.
unsafe impl Send for SettingsContext {}

static SETTINGS: OnceLock<Mutex<Option<SettingsContext>>> = OnceLock::new();

pub fn open(
    owner: HWND,
    config: AppConfig,
    endpoints: Vec<Endpoint>,
    save: SaveCallback,
) -> Result<()> {
    let storage = SETTINGS.get_or_init(|| Mutex::new(None));
    if let Some(context) = storage.lock().expect("settings mutex poisoned").as_ref() {
        unsafe {
            let _ = SetForegroundWindow(context.window);
        }
        return Ok(());
    }

    let module = unsafe { GetModuleHandleW(None) }.context("getting settings module")?;
    let instance = HINSTANCE(module.0);
    let class = WNDCLASSW {
        hInstance: instance,
        lpszClassName: w!("AudioSwitch.SettingsWindow.v1"),
        lpfnWndProc: Some(window_proc),
        ..Default::default()
    };
    if unsafe { RegisterClassW(&class) } == 0 && unsafe { GetLastError().0 } != 1410 {
        return Err(anyhow!("registering settings window failed"));
    }
    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class.lpszClassName,
            w!("Audio Switch settings"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            620,
            480,
            Some(owner),
            None,
            Some(instance),
            None,
        )
    }
    .context("creating settings window")?;

    label(window, instance, 22, 22, 170, "Primary name")?;
    let primary_name = edit(
        window,
        instance,
        ID_PRIMARY_NAME,
        200,
        18,
        385,
        &config.labels.primary,
    )?;
    label(window, instance, 22, 58, 170, "Secondary name")?;
    let secondary_name = edit(
        window,
        instance,
        ID_SECONDARY_NAME,
        200,
        54,
        385,
        &config.labels.secondary,
    )?;

    label(window, instance, 22, 104, 170, "Primary playback")?;
    let primary_render = combo(
        window,
        instance,
        ID_PRIMARY_RENDER,
        200,
        100,
        &endpoints,
        EndpointFlow::Render,
        config.endpoints.primary_render.as_deref(),
    )?;
    label(window, instance, 22, 140, 170, "Secondary playback")?;
    let secondary_render = combo(
        window,
        instance,
        ID_SECONDARY_RENDER,
        200,
        136,
        &endpoints,
        EndpointFlow::Render,
        config.endpoints.secondary_render.as_deref(),
    )?;
    label(window, instance, 22, 176, 170, "Primary microphone")?;
    let primary_capture = combo(
        window,
        instance,
        ID_PRIMARY_CAPTURE,
        200,
        172,
        &endpoints,
        EndpointFlow::Capture,
        config.endpoints.primary_capture.as_deref(),
    )?;
    label(window, instance, 22, 212, 170, "Secondary microphone")?;
    let secondary_capture = combo(
        window,
        instance,
        ID_SECONDARY_CAPTURE,
        200,
        208,
        &endpoints,
        EndpointFlow::Capture,
        config.endpoints.secondary_capture.as_deref(),
    )?;
    label(window, instance, 22, 248, 170, "Virtual microphone input")?;
    let virtual_render = combo(
        window,
        instance,
        ID_VIRTUAL_RENDER,
        200,
        244,
        &endpoints,
        EndpointFlow::Render,
        config.endpoints.virtual_render.as_deref(),
    )?;

    let cue_enabled = checkbox(
        window,
        instance,
        ID_CUES,
        22,
        304,
        "Play a cue when switching",
        config.audio.cue_enabled,
    )?;
    let auto_start = checkbox(
        window,
        instance,
        ID_AUTOSTART,
        310,
        304,
        "Start at Windows sign-in",
        config.auto_start,
    )?;
    let auto_start_elevated = checkbox(
        window,
        instance,
        ID_AUTOSTART_ELEVATED,
        310,
        338,
        "Require UAC prompt and high priority",
        config.auto_start_elevated,
    )?;
    button(window, instance, ID_SAVE, 390, 388, 92, "Save")?;
    button(window, instance, ID_CANCEL, 493, 388, 92, "Cancel")?;

    *storage.lock().expect("settings mutex poisoned") = Some(SettingsContext {
        window,
        primary_name,
        secondary_name,
        primary_render,
        secondary_render,
        primary_capture,
        secondary_capture,
        virtual_render,
        cue_enabled,
        auto_start,
        auto_start_elevated,
        config,
        save,
    });
    unsafe {
        let _ = ShowWindow(window, SW_SHOW);
    }
    Ok(())
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_COMMAND => {
            match wparam.0 & 0xffff {
                ID_SAVE => match save_settings() {
                    Ok(()) => unsafe {
                        let _ = DestroyWindow(window);
                    },
                    Err(error) => show_error(window, &error.to_string()),
                },
                ID_CANCEL => unsafe {
                    let _ = DestroyWindow(window);
                },
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(window);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(storage) = SETTINGS.get() {
                *storage.lock().expect("settings mutex poisoned") = None;
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

fn save_settings() -> Result<()> {
    let storage = SETTINGS.get().context("settings storage is unavailable")?;
    let context = storage.lock().expect("settings mutex poisoned");
    let context = context.as_ref().context("settings window is unavailable")?;
    let mut config = context.config.clone();
    config.labels.primary = non_empty_name(read_text(context.primary_name), "Primary");
    config.labels.secondary = non_empty_name(read_text(context.secondary_name), "Secondary");
    config.endpoints.primary_render = selection(&context.primary_render);
    config.endpoints.secondary_render = selection(&context.secondary_render);
    config.endpoints.primary_capture = selection(&context.primary_capture);
    config.endpoints.secondary_capture = selection(&context.secondary_capture);
    config.endpoints.virtual_render = selection(&context.virtual_render);
    config.audio.cue_enabled = checked(context.cue_enabled);
    config.auto_start = checked(context.auto_start);
    config.auto_start_elevated = checked(context.auto_start_elevated);
    (context.save)(config)
}

#[allow(clippy::too_many_arguments)]
fn combo(
    parent: HWND,
    instance: HINSTANCE,
    id: usize,
    x: i32,
    y: i32,
    endpoints: &[Endpoint],
    flow: EndpointFlow,
    selected: Option<&str>,
) -> Result<ComboBinding> {
    let window = control(
        parent,
        instance,
        w!("COMBOBOX"),
        PCWSTR::null(),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_BORDER,
        id,
        x,
        y,
        385,
        240,
    )?;
    let mut values = vec![None];
    add_combo_string(window, "Not selected");
    let mut selected_index = 0;
    for endpoint in endpoints.iter().filter(|endpoint| endpoint.flow == flow) {
        values.push(Some(endpoint.id.clone()));
        add_combo_string(window, &endpoint.name);
        if selected.is_some_and(|id| id.eq_ignore_ascii_case(&endpoint.id)) {
            selected_index = values.len() - 1;
        }
    }
    if let Some(selected) = selected
        && selected_index == 0
    {
        values.push(Some(selected.to_owned()));
        add_combo_string(window, "Unavailable selected endpoint");
        selected_index = values.len() - 1;
    }
    unsafe {
        let _ = SendMessageW(
            window,
            CB_SETCURSEL,
            Some(WPARAM(selected_index)),
            Some(LPARAM(0)),
        );
    }
    Ok(ComboBinding { window, values })
}

fn selection(binding: &ComboBinding) -> Option<String> {
    let index = unsafe { SendMessageW(binding.window, CB_GETCURSEL, None, None) }.0;
    usize::try_from(index)
        .ok()
        .and_then(|index| binding.values.get(index))
        .cloned()
        .flatten()
}

fn edit(
    parent: HWND,
    instance: HINSTANCE,
    id: usize,
    x: i32,
    y: i32,
    width: i32,
    value: &str,
) -> Result<HWND> {
    let value = wide_null(value);
    control(
        parent,
        instance,
        w!("EDIT"),
        PCWSTR(value.as_ptr()),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
        id,
        x,
        y,
        width,
        25,
    )
}

fn label(parent: HWND, instance: HINSTANCE, x: i32, y: i32, width: i32, text: &str) -> Result<()> {
    let text = wide_null(text);
    control(
        parent,
        instance,
        w!("STATIC"),
        PCWSTR(text.as_ptr()),
        WS_CHILD | WS_VISIBLE,
        0,
        x,
        y,
        width,
        24,
    )?;
    Ok(())
}

fn checkbox(
    parent: HWND,
    instance: HINSTANCE,
    id: usize,
    x: i32,
    y: i32,
    text: &str,
    enabled: bool,
) -> Result<HWND> {
    let text = wide_null(text);
    let window = control(
        parent,
        instance,
        w!("BUTTON"),
        PCWSTR(text.as_ptr()),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
        id,
        x,
        y,
        260,
        26,
    )?;
    unsafe {
        let _ = SendMessageW(
            window,
            BM_SETCHECK,
            Some(WPARAM(usize::from(enabled))),
            Some(LPARAM(0)),
        );
    }
    Ok(window)
}

fn button(
    parent: HWND,
    instance: HINSTANCE,
    id: usize,
    x: i32,
    y: i32,
    width: i32,
    text: &str,
) -> Result<()> {
    let text = wide_null(text);
    control(
        parent,
        instance,
        w!("BUTTON"),
        PCWSTR(text.as_ptr()),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP,
        id,
        x,
        y,
        width,
        30,
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn control(
    parent: HWND,
    instance: HINSTANCE,
    class: PCWSTR,
    text: PCWSTR,
    style: WINDOW_STYLE,
    id: usize,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Result<HWND> {
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            text,
            style,
            x,
            y,
            width,
            height,
            Some(parent),
            Some(HMENU(id as *mut c_void)),
            Some(instance),
            None,
        )
    }
    .context("creating settings control")
}

fn add_combo_string(window: HWND, value: &str) {
    let value = wide_null(value);
    unsafe {
        let _ = SendMessageW(
            window,
            CB_ADDSTRING,
            None,
            Some(LPARAM(value.as_ptr() as isize)),
        );
    }
}

fn read_text(window: HWND) -> String {
    let length = unsafe { GetWindowTextLengthW(window) }.max(0) as usize;
    let mut value = vec![0_u16; length + 1];
    let copied = unsafe { GetWindowTextW(window, &mut value) }.max(0) as usize;
    String::from_utf16_lossy(&value[..copied])
}

fn checked(window: HWND) -> bool {
    unsafe { SendMessageW(window, BM_GETCHECK, None, None) }.0 == 1
}

fn non_empty_name(value: String, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value.chars().take(24).collect()
    }
}

fn show_error(window: HWND, message: &str) {
    let message = wide_null(message);
    unsafe {
        MessageBoxW(
            Some(window),
            PCWSTR(message.as_ptr()),
            w!("Audio Switch settings"),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

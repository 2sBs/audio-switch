use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, anyhow};
use audio_switch_core::AppConfig;
use audio_switch_core::audio::{Endpoint, EndpointFlow};
use windows::Win32::Foundation::{GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BLACK_BRUSH, BeginPaint, EndPaint, FillRect, GetStockObject, HBRUSH, HDC, PAINTSTRUCT,
    SetBkColor, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::DRAWITEMSTRUCT;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_OWNERDRAW, CB_ADDSTRING, CB_GETCURSEL,
    CB_RESETCONTENT, CB_SETCURSEL, CBS_DROPDOWNLIST, CW_USEDEFAULT, CreateWindowExW,
    DefWindowProcW, DestroyWindow, DispatchMessageW, ES_AUTOHSCROLL, GetClientRect, GetMessageW,
    GetWindowTextLengthW, GetWindowTextW, HMENU, IDYES, MB_ICONERROR, MB_ICONWARNING, MB_OK,
    MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO, MSG, MessageBoxW, PostQuitMessage, RegisterClassW,
    SW_HIDE, SW_SHOW, SW_SHOWNORMAL, SendMessageW, SetWindowTextW, ShowWindow, TranslateMessage,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CTLCOLORBTN, WM_CTLCOLOREDIT,
    WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DRAWITEM, WM_PAINT, WNDCLASSW, WS_BORDER,
    WS_CAPTION, WS_CHILD, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};
use windows::core::{PCWSTR, w};

use crate::ui::{self, ButtonStyle, Theme};

pub const DRIVER_DOWNLOAD_URL: &str = "https://vb-audio.com/Cable/";

const STEP_COUNT: usize = 5;
const ID_VIRTUAL_RENDER: usize = 200;
const ID_PRIMARY_RENDER: usize = 201;
const ID_SECONDARY_RENDER: usize = 202;
const ID_PRIMARY_CAPTURE: usize = 203;
const ID_SECONDARY_CAPTURE: usize = 204;
const ID_PRIMARY_NAME: usize = 205;
const ID_SECONDARY_NAME: usize = 206;
const ID_CUES: usize = 207;
const ID_AUTOSTART: usize = 208;
const ID_AUTOSTART_ELEVATED: usize = 209;
const ID_GET_DRIVER: usize = 300;
const ID_REFRESH: usize = 301;
const ID_BACK: usize = 400;
const ID_NEXT: usize = 401;
const ID_CANCEL: usize = 402;

const STEP_TITLES: [&str; STEP_COUNT] = [
    "Virtual cable",
    "Playback",
    "Microphones",
    "Verify",
    "Startup",
];

type RefreshCallback = fn() -> Result<Vec<Endpoint>>;

#[derive(Debug)]
struct ComboBinding {
    window: HWND,
    values: Vec<Option<String>>,
}

struct SetupContext {
    step: usize,
    rail_heading: HWND,
    step_labels: [HWND; STEP_COUNT],
    heading: HWND,
    description: HWND,
    status: HWND,
    step_controls: [Vec<HWND>; STEP_COUNT],
    virtual_render: ComboBinding,
    primary_render: ComboBinding,
    secondary_render: ComboBinding,
    primary_capture: ComboBinding,
    secondary_capture: ComboBinding,
    primary_name: HWND,
    secondary_name: HWND,
    cue_enabled: HWND,
    auto_start: HWND,
    auto_start_elevated: HWND,
    verification: HWND,
    back: HWND,
    next: HWND,
    config: AppConfig,
    endpoints: Vec<Endpoint>,
    refresh: RefreshCallback,
    theme: Theme,
}

struct ScreenUpdate {
    visibility: Vec<(HWND, bool)>,
    step_labels: Vec<(HWND, String)>,
    heading: (HWND, &'static str),
    description: (HWND, &'static str),
    status: (HWND, String),
    verification: Option<(HWND, String)>,
    back: (HWND, bool),
    next: (HWND, &'static str, bool),
}

// All setup-window access occurs on the thread running its Win32 message loop.
unsafe impl Send for SetupContext {}

static SETUP: OnceLock<Mutex<Option<SetupContext>>> = OnceLock::new();
static SETUP_RESULT: OnceLock<Mutex<Option<AppConfig>>> = OnceLock::new();

pub fn virtual_cable_missing(config: &AppConfig, endpoints: &[Endpoint]) -> bool {
    let Some(selected) = config.endpoints.virtual_render.as_deref() else {
        return false;
    };
    !endpoint_is_available(endpoints, selected, EndpointFlow::Render)
}

pub fn prompt_for_missing_virtual_cable() -> bool {
    unsafe {
        MessageBoxW(
            None,
            w!(
                "The configured virtual audio cable is unavailable. Audio Switch can wait for it to return, or open setup so you can install or select a cable now.\r\n\r\nOpen setup?"
            ),
            w!("Virtual cable unavailable"),
            MB_YESNO | MB_ICONWARNING | MB_SETFOREGROUND | MB_TOPMOST,
        ) == IDYES
    }
}

pub fn run(
    mut config: AppConfig,
    endpoints: Vec<Endpoint>,
    refresh: RefreshCallback,
) -> Result<Option<AppConfig>> {
    let storage = SETUP.get_or_init(|| Mutex::new(None));
    if storage.lock().expect("setup mutex poisoned").is_some() {
        return Err(anyhow!("setup is already open"));
    }
    *SETUP_RESULT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("setup result mutex poisoned") = None;

    if config.endpoints.virtual_render.is_none()
        && let Some(endpoint) = preferred_virtual_cable(&endpoints)
    {
        config.endpoints.virtual_render = Some(endpoint.id.clone());
    }

    let module = unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None) }
        .context("getting setup module")?;
    let instance = HINSTANCE(module.0);
    let black_brush = HBRUSH(unsafe { GetStockObject(BLACK_BRUSH) }.0);
    let class = WNDCLASSW {
        hInstance: instance,
        hIcon: ui::app_icon(instance),
        lpszClassName: w!("AudioSwitch.SetupWindow.v1"),
        lpfnWndProc: Some(window_proc),
        hbrBackground: black_brush,
        ..Default::default()
    };
    if unsafe { RegisterClassW(&class) } == 0 && unsafe { GetLastError().0 } != 1410 {
        return Err(anyhow!("registering setup window failed"));
    }

    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class.lpszClassName,
            w!("Audio Switch setup"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            840,
            620,
            None,
            None,
            Some(instance),
            None,
        )
    }
    .context("creating setup window")?;
    ui::use_dark_title_bar(window);
    let theme = Theme::new()?;

    let rail_heading = label(window, instance, 24, 28, 180, 34, "Set up Audio Switch")?;
    let step_labels = std::array::from_fn(|index| {
        label(
            window,
            instance,
            24,
            90 + (index as i32 * 42),
            178,
            28,
            STEP_TITLES[index],
        )
        .expect("creating setup step label")
    });
    let heading = label(window, instance, 242, 34, 550, 38, "")?;
    let description = label(window, instance, 242, 78, 550, 58, "")?;
    let status = label(window, instance, 255, 146, 537, 38, "")?;

    let virtual_label = label(
        window,
        instance,
        242,
        205,
        190,
        24,
        "Virtual render endpoint",
    )?;
    let virtual_render = combo(
        window,
        instance,
        ID_VIRTUAL_RENDER,
        242,
        230,
        540,
        &endpoints,
        EndpointFlow::Render,
        config.endpoints.virtual_render.as_deref(),
    )?;
    let driver = button(
        window,
        instance,
        ID_GET_DRIVER,
        242,
        290,
        176,
        "Get virtual cable",
        false,
    )?;
    let refresh_virtual = button(
        window,
        instance,
        ID_REFRESH,
        430,
        290,
        138,
        "Refresh devices",
        false,
    )?;
    let driver_note = label(
        window,
        instance,
        242,
        342,
        540,
        58,
        "The download opens in your browser. Audio Switch does not download, install, or redistribute the driver.",
    )?;

    let primary_render_label = label(window, instance, 242, 196, 190, 24, "Primary playback")?;
    let primary_render = combo(
        window,
        instance,
        ID_PRIMARY_RENDER,
        242,
        221,
        540,
        &endpoints,
        EndpointFlow::Render,
        config.endpoints.primary_render.as_deref(),
    )?;
    let secondary_render_label = label(window, instance, 242, 286, 190, 24, "Secondary playback")?;
    let secondary_render = combo(
        window,
        instance,
        ID_SECONDARY_RENDER,
        242,
        311,
        540,
        &endpoints,
        EndpointFlow::Render,
        config.endpoints.secondary_render.as_deref(),
    )?;

    let primary_name_label = label(window, instance, 242, 196, 150, 24, "Primary name")?;
    let primary_name = edit(
        window,
        instance,
        ID_PRIMARY_NAME,
        400,
        192,
        382,
        &config.labels.primary,
    )?;
    let primary_capture_label = label(window, instance, 242, 256, 150, 24, "Primary microphone")?;
    let primary_capture = combo(
        window,
        instance,
        ID_PRIMARY_CAPTURE,
        400,
        252,
        382,
        &endpoints,
        EndpointFlow::Capture,
        config.endpoints.primary_capture.as_deref(),
    )?;
    let secondary_name_label = label(window, instance, 242, 326, 150, 24, "Secondary name")?;
    let secondary_name = edit(
        window,
        instance,
        ID_SECONDARY_NAME,
        400,
        322,
        382,
        &config.labels.secondary,
    )?;
    let secondary_capture_label =
        label(window, instance, 242, 386, 150, 24, "Secondary microphone")?;
    let secondary_capture = combo(
        window,
        instance,
        ID_SECONDARY_CAPTURE,
        400,
        382,
        382,
        &endpoints,
        EndpointFlow::Capture,
        config.endpoints.secondary_capture.as_deref(),
    )?;

    let verification = label(window, instance, 242, 196, 540, 208, "")?;
    let refresh_verify = button(
        window,
        instance,
        ID_REFRESH,
        242,
        416,
        138,
        "Refresh devices",
        false,
    )?;

    let cue_enabled = checkbox(
        window,
        instance,
        ID_CUES,
        242,
        194,
        430,
        "Play a cue when switching microphones",
        config.audio.cue_enabled,
    )?;
    let auto_start = checkbox(
        window,
        instance,
        ID_AUTOSTART,
        242,
        246,
        430,
        "Start Audio Switch at Windows sign-in",
        config.auto_start,
    )?;
    let auto_start_elevated = checkbox(
        window,
        instance,
        ID_AUTOSTART_ELEVATED,
        242,
        298,
        500,
        "Request UAC elevation and use high process priority",
        config.auto_start_elevated,
    )?;
    let startup_note = label(
        window,
        instance,
        242,
        352,
        540,
        58,
        "Elevated startup shows a UAC prompt at every sign-in. Standard startup is recommended for most systems.",
    )?;

    let back = button(window, instance, ID_BACK, 506, 520, 84, "Back", false)?;
    let next = button(window, instance, ID_NEXT, 600, 520, 96, "Continue", true)?;
    let cancel = button(window, instance, ID_CANCEL, 706, 520, 76, "Exit", false)?;

    theme.set_semibold(rail_heading);
    theme.set_heading(heading);
    theme.set_small(description);
    theme.set_small(status);
    for step_label in step_labels {
        theme.set_body(step_label);
    }
    for control in [
        virtual_render.window,
        primary_render.window,
        secondary_render.window,
        primary_capture.window,
        secondary_capture.window,
        primary_name,
        secondary_name,
        cue_enabled,
        auto_start,
        auto_start_elevated,
        driver,
        refresh_virtual,
        refresh_verify,
        back,
        next,
        cancel,
    ] {
        theme.set_body(control);
        ui::use_dark_control(control);
    }

    let step_controls = [
        vec![
            virtual_label,
            virtual_render.window,
            driver,
            refresh_virtual,
            driver_note,
        ],
        vec![
            primary_render_label,
            primary_render.window,
            secondary_render_label,
            secondary_render.window,
        ],
        vec![
            primary_name_label,
            primary_name,
            primary_capture_label,
            primary_capture.window,
            secondary_name_label,
            secondary_name,
            secondary_capture_label,
            secondary_capture.window,
        ],
        vec![verification, refresh_verify],
        vec![cue_enabled, auto_start, auto_start_elevated, startup_note],
    ];

    *storage.lock().expect("setup mutex poisoned") = Some(SetupContext {
        step: 0,
        rail_heading,
        step_labels,
        heading,
        description,
        status,
        step_controls,
        virtual_render,
        primary_render,
        secondary_render,
        primary_capture,
        secondary_capture,
        primary_name,
        secondary_name,
        cue_enabled,
        auto_start,
        auto_start_elevated,
        verification,
        back,
        next,
        config,
        endpoints,
        refresh,
        theme,
    });

    update_screen()?;
    unsafe {
        let _ = ShowWindow(window, SW_SHOW);
    }

    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    Ok(SETUP_RESULT
        .get()
        .context("setup result storage is unavailable")?
        .lock()
        .expect("setup result mutex poisoned")
        .take())
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
                ID_BACK => navigate_back(),
                ID_NEXT => navigate_next(window),
                ID_CANCEL => unsafe {
                    let _ = DestroyWindow(window);
                },
                ID_GET_DRIVER => open_driver_download(window),
                ID_REFRESH => refresh_endpoints(window),
                ID_VIRTUAL_RENDER
                | ID_PRIMARY_RENDER
                | ID_SECONDARY_RENDER
                | ID_PRIMARY_CAPTURE
                | ID_SECONDARY_CAPTURE
                | ID_CUES
                | ID_AUTOSTART
                | ID_AUTOSTART_ELEVATED => {
                    let _ = update_screen();
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_DRAWITEM => {
            let item = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
            if let Some(storage) = SETUP.get()
                && let Some(context) = storage.lock().expect("setup mutex poisoned").as_ref()
            {
                let style = if item.CtlID as usize == ID_NEXT {
                    ButtonStyle::Primary
                } else {
                    ButtonStyle::Standard
                };
                ui::draw_button(item, &context.theme, style);
                return LRESULT(1);
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let device_context = HDC(wparam.0 as *mut c_void);
            let control = HWND(lparam.0 as *mut c_void);
            if let Some(storage) = SETUP.get()
                && let Some(context) = storage.lock().expect("setup mutex poisoned").as_ref()
            {
                let step_index = context
                    .step_labels
                    .iter()
                    .position(|label| *label == control);
                let rail_control = control == context.rail_heading || step_index.is_some();
                let text = match step_index {
                    Some(index) if index == context.step => ui::ACCENT,
                    Some(index) if index < context.step => ui::MUTED,
                    Some(_) => ui::QUIET,
                    None if control == context.description => ui::MUTED,
                    None if control == context.status => ui::SUCCESS,
                    None => ui::TEXT,
                };
                let brush = if message == WM_CTLCOLOREDIT || message == WM_CTLCOLORLISTBOX {
                    context.theme.control
                } else if rail_control {
                    context.theme.panel
                } else {
                    context.theme.background
                };
                unsafe {
                    let _ = SetBkMode(device_context, TRANSPARENT);
                    SetBkColor(device_context, ui::CONTROL);
                    SetTextColor(device_context, text);
                }
                return LRESULT(brush.0 as isize);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let device_context = unsafe { BeginPaint(window, &mut paint) };
            if let Some(storage) = SETUP.get()
                && let Some(context) = storage.lock().expect("setup mutex poisoned").as_ref()
            {
                let mut client = RECT::default();
                unsafe {
                    let _ = GetClientRect(window, &mut client);
                    let rail = RECT {
                        left: 0,
                        top: 0,
                        right: 213,
                        bottom: client.bottom,
                    };
                    let divider = RECT {
                        left: 212,
                        top: 0,
                        right: 213,
                        bottom: client.bottom,
                    };
                    let marker_top = 90 + context.step as i32 * 42;
                    let marker = RECT {
                        left: 0,
                        top: marker_top,
                        right: 2,
                        bottom: marker_top + 28,
                    };
                    let status = RECT {
                        left: 242,
                        top: 146,
                        right: 244,
                        bottom: 184,
                    };
                    let _ = FillRect(device_context, &rail, context.theme.panel);
                    let _ = FillRect(device_context, &divider, context.theme.line);
                    let _ = FillRect(device_context, &marker, context.theme.accent);
                    let _ = FillRect(device_context, &status, context.theme.accent);
                }
            }
            unsafe {
                let _ = EndPaint(window, &paint);
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
            if let Some(storage) = SETUP.get() {
                *storage.lock().expect("setup mutex poisoned") = None;
            }
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

fn navigate_back() {
    let Some(storage) = SETUP.get() else {
        return;
    };
    {
        let mut context = storage.lock().expect("setup mutex poisoned");
        let Some(context) = context.as_mut() else {
            return;
        };
        capture_config(context);
        context.step = context.step.saturating_sub(1);
    }
    let _ = update_screen();
}

fn navigate_next(window: HWND) {
    let Some(storage) = SETUP.get() else {
        return;
    };
    let outcome = {
        let mut storage = storage.lock().expect("setup mutex poisoned");
        let Some(context) = storage.as_mut() else {
            return;
        };
        capture_config(context);
        match step_validation(context) {
            Err(message) => Err(message),
            Ok(()) if context.step + 1 < STEP_COUNT => {
                context.step += 1;
                Ok(None)
            }
            Ok(()) => Ok(Some(context.config.clone())),
        }
    };

    match outcome {
        Err(message) => show_error(window, &message),
        Ok(Some(config)) => {
            if let Some(result) = SETUP_RESULT.get() {
                *result.lock().expect("setup result mutex poisoned") = Some(config);
            }
            unsafe {
                let _ = DestroyWindow(window);
            }
        }
        Ok(None) => {
            let _ = update_screen();
        }
    }
}

fn refresh_endpoints(window: HWND) {
    let Some(storage) = SETUP.get() else {
        return;
    };
    let result = {
        let mut storage = storage.lock().expect("setup mutex poisoned");
        let Some(context) = storage.as_mut() else {
            return;
        };
        capture_config(context);
        (context.refresh)().map(|endpoints| {
            context.endpoints = endpoints;
            if context.config.endpoints.virtual_render.is_none()
                && let Some(endpoint) = preferred_virtual_cable(&context.endpoints)
            {
                context.config.endpoints.virtual_render = Some(endpoint.id.clone());
            }
            repopulate_combos(context);
        })
    };

    match result {
        Ok(()) => {
            let _ = update_screen();
        }
        Err(error) => show_error(
            window,
            &format!("Could not refresh audio devices: {error:#}"),
        ),
    }
}

fn update_screen() -> Result<()> {
    let storage = SETUP.get().context("setup storage is unavailable")?;
    let screen = {
        let mut storage = storage.lock().expect("setup mutex poisoned");
        let context = storage.as_mut().context("setup window is unavailable")?;
        capture_config(context);
        let step = context.step;

        let visibility = context
            .step_controls
            .iter()
            .enumerate()
            .flat_map(|(index, controls)| {
                controls
                    .iter()
                    .copied()
                    .map(move |control| (control, index == step))
            })
            .collect();
        let step_labels = context
            .step_labels
            .iter()
            .enumerate()
            .map(|(index, label)| {
                let marker = if index < step {
                    "✓"
                } else if index == step {
                    "●"
                } else {
                    "○"
                };
                (*label, format!("{marker}  {}", STEP_TITLES[index]))
            })
            .collect();

        let (heading, description, status, verification) = match step {
            0 => (
                "Connect a virtual audio cable",
                "Audio Switch keeps one stable microphone in games and calls while changing the physical source behind it.",
                virtual_status(context),
                None,
            ),
            1 => (
                "Choose playback devices",
                "Audio from Primary is mirrored to Secondary while both endpoints are available.",
                selection_status(
                    &context.config.endpoints.primary_render,
                    &context.config.endpoints.secondary_render,
                    "Select both playback endpoints.",
                ),
                None,
            ),
            2 => (
                "Choose microphones",
                "Primary and Secondary are editable roles. Audio Switch never depends on a physical device name.",
                selection_status(
                    &context.config.endpoints.primary_capture,
                    &context.config.endpoints.secondary_capture,
                    "Select both microphone endpoints.",
                ),
                None,
            ),
            3 => {
                let status = match validate_configuration(&context.config, &context.endpoints) {
                    Ok(()) => "All five endpoints are online and ready.".to_owned(),
                    Err(message) => message,
                };
                (
                    "Verify the audio route",
                    "Review the selected endpoints. Refresh after connecting or enabling a device.",
                    status,
                    Some((context.verification, verification_summary(context))),
                )
            }
            _ => (
                "Choose startup behavior",
                "These preferences can be changed later from the tray settings window.",
                "Setup is ready to save.".to_owned(),
                None,
            ),
        };

        ScreenUpdate {
            visibility,
            step_labels,
            heading: (context.heading, heading),
            description: (context.description, description),
            status: (context.status, status),
            verification,
            back: (context.back, step > 0),
            next: (
                context.next,
                if step + 1 == STEP_COUNT {
                    "Finish"
                } else {
                    "Continue"
                },
                step_validation(context).is_ok(),
            ),
        }
    };

    for (control, visible) in screen.visibility {
        unsafe {
            let _ = ShowWindow(control, if visible { SW_SHOW } else { SW_HIDE });
        }
    }
    for (label, text) in screen.step_labels {
        set_text(label, &text);
    }
    if let Some((verification, text)) = screen.verification {
        set_text(verification, &text);
    }
    set_text(screen.heading.0, screen.heading.1);
    set_text(screen.description.0, screen.description.1);
    set_text(screen.status.0, &screen.status.1);
    set_text(screen.next.0, screen.next.1);
    unsafe {
        let _ = EnableWindow(screen.back.0, screen.back.1);
        let _ = EnableWindow(screen.next.0, screen.next.2);
    }
    Ok(())
}

fn step_validation(context: &SetupContext) -> std::result::Result<(), String> {
    match context.step {
        0 => validate_selection(
            &context.config.endpoints.virtual_render,
            &context.endpoints,
            EndpointFlow::Render,
            "Select an available virtual cable render endpoint.",
        ),
        1 => {
            validate_selection(
                &context.config.endpoints.primary_render,
                &context.endpoints,
                EndpointFlow::Render,
                "Select an available Primary playback endpoint.",
            )?;
            validate_selection(
                &context.config.endpoints.secondary_render,
                &context.endpoints,
                EndpointFlow::Render,
                "Select an available Secondary playback endpoint.",
            )
        }
        2 => {
            validate_selection(
                &context.config.endpoints.primary_capture,
                &context.endpoints,
                EndpointFlow::Capture,
                "Select an available Primary microphone.",
            )?;
            validate_selection(
                &context.config.endpoints.secondary_capture,
                &context.endpoints,
                EndpointFlow::Capture,
                "Select an available Secondary microphone.",
            )
        }
        3 => validate_configuration(&context.config, &context.endpoints),
        _ => Ok(()),
    }
}

fn validate_configuration(
    config: &AppConfig,
    endpoints: &[Endpoint],
) -> std::result::Result<(), String> {
    let required = [
        (
            &config.endpoints.virtual_render,
            EndpointFlow::Render,
            "Virtual cable",
        ),
        (
            &config.endpoints.primary_render,
            EndpointFlow::Render,
            "Primary playback",
        ),
        (
            &config.endpoints.secondary_render,
            EndpointFlow::Render,
            "Secondary playback",
        ),
        (
            &config.endpoints.primary_capture,
            EndpointFlow::Capture,
            "Primary microphone",
        ),
        (
            &config.endpoints.secondary_capture,
            EndpointFlow::Capture,
            "Secondary microphone",
        ),
    ];
    for (selected, flow, label) in required {
        validate_selection(
            selected,
            endpoints,
            flow,
            &format!("{label} is missing or unavailable."),
        )?;
    }
    Ok(())
}

fn validate_selection(
    selected: &Option<String>,
    endpoints: &[Endpoint],
    flow: EndpointFlow,
    message: &str,
) -> std::result::Result<(), String> {
    if selected
        .as_deref()
        .is_some_and(|id| endpoint_is_available(endpoints, id, flow))
    {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}

fn endpoint_is_available(endpoints: &[Endpoint], id: &str, flow: EndpointFlow) -> bool {
    endpoints.iter().any(|endpoint| {
        endpoint.flow == flow && endpoint.available && endpoint.id.eq_ignore_ascii_case(id)
    })
}

fn recognized_virtual_cable(endpoint: &Endpoint) -> bool {
    if endpoint.flow != EndpointFlow::Render || !endpoint.available {
        return false;
    }
    let name = endpoint.name.to_ascii_lowercase();
    name.contains("cable input")
        || (name.contains("vb-audio") && name.contains("cable"))
        || name.contains("virtual audio cable")
}

fn preferred_virtual_cable(endpoints: &[Endpoint]) -> Option<&Endpoint> {
    endpoints
        .iter()
        .filter(|endpoint| recognized_virtual_cable(endpoint))
        .min_by_key(|endpoint| {
            let name = endpoint.name.to_ascii_lowercase();
            usize::from(!name.starts_with("cable input"))
        })
}

fn virtual_status(context: &SetupContext) -> String {
    let Some(selected) = context.config.endpoints.virtual_render.as_deref() else {
        return "No compatible virtual cable was detected. Install one or select an existing compatible endpoint.".into();
    };
    match context
        .endpoints
        .iter()
        .find(|endpoint| endpoint.id.eq_ignore_ascii_case(selected))
    {
        Some(endpoint) if endpoint.available && recognized_virtual_cable(endpoint) => {
            format!("Detected and selected {}.", endpoint.name)
        }
        Some(endpoint) if endpoint.available => format!("Selected {}.", endpoint.name),
        _ => "The selected virtual cable is unavailable. Install it or refresh the device list."
            .into(),
    }
}

fn selection_status(first: &Option<String>, second: &Option<String>, missing: &str) -> String {
    if first.is_some() && second.is_some() {
        "Both endpoints are selected.".into()
    } else {
        missing.into()
    }
}

fn verification_summary(context: &SetupContext) -> String {
    let line = |label: &str, selected: &Option<String>| {
        let value = selected
            .as_deref()
            .and_then(|id| {
                context
                    .endpoints
                    .iter()
                    .find(|endpoint| endpoint.id.eq_ignore_ascii_case(id))
            })
            .map(|endpoint| endpoint.name.as_str())
            .unwrap_or("Unavailable or not selected");
        format!("{label}:\r\n  {value}")
    };
    [
        line("Virtual cable", &context.config.endpoints.virtual_render),
        line("Primary playback", &context.config.endpoints.primary_render),
        line(
            "Secondary playback",
            &context.config.endpoints.secondary_render,
        ),
        line(
            "Primary microphone",
            &context.config.endpoints.primary_capture,
        ),
        line(
            "Secondary microphone",
            &context.config.endpoints.secondary_capture,
        ),
    ]
    .join("\r\n\r\n")
}

fn capture_config(context: &mut SetupContext) {
    context.config.endpoints.virtual_render = selection(&context.virtual_render);
    context.config.endpoints.primary_render = selection(&context.primary_render);
    context.config.endpoints.secondary_render = selection(&context.secondary_render);
    context.config.endpoints.primary_capture = selection(&context.primary_capture);
    context.config.endpoints.secondary_capture = selection(&context.secondary_capture);
    context.config.labels.primary = non_empty_name(read_text(context.primary_name), "Primary");
    context.config.labels.secondary =
        non_empty_name(read_text(context.secondary_name), "Secondary");
    context.config.audio.cue_enabled = checked(context.cue_enabled);
    context.config.auto_start = checked(context.auto_start);
    context.config.auto_start_elevated = checked(context.auto_start_elevated);
}

fn repopulate_combos(context: &mut SetupContext) {
    repopulate_combo(
        &mut context.virtual_render,
        &context.endpoints,
        EndpointFlow::Render,
        context.config.endpoints.virtual_render.as_deref(),
    );
    repopulate_combo(
        &mut context.primary_render,
        &context.endpoints,
        EndpointFlow::Render,
        context.config.endpoints.primary_render.as_deref(),
    );
    repopulate_combo(
        &mut context.secondary_render,
        &context.endpoints,
        EndpointFlow::Render,
        context.config.endpoints.secondary_render.as_deref(),
    );
    repopulate_combo(
        &mut context.primary_capture,
        &context.endpoints,
        EndpointFlow::Capture,
        context.config.endpoints.primary_capture.as_deref(),
    );
    repopulate_combo(
        &mut context.secondary_capture,
        &context.endpoints,
        EndpointFlow::Capture,
        context.config.endpoints.secondary_capture.as_deref(),
    );
}

fn repopulate_combo(
    binding: &mut ComboBinding,
    endpoints: &[Endpoint],
    flow: EndpointFlow,
    selected: Option<&str>,
) {
    unsafe {
        let _ = SendMessageW(binding.window, CB_RESETCONTENT, None, None);
    }
    binding.values.clear();
    binding.values.push(None);
    add_combo_string(binding.window, "Not selected");
    let mut selected_index = 0;
    for endpoint in endpoints
        .iter()
        .filter(|endpoint| endpoint.flow == flow && endpoint.available)
    {
        binding.values.push(Some(endpoint.id.clone()));
        add_combo_string(binding.window, &endpoint.name);
        if selected.is_some_and(|id| id.eq_ignore_ascii_case(&endpoint.id)) {
            selected_index = binding.values.len() - 1;
        }
    }
    if let Some(selected) = selected
        && selected_index == 0
    {
        binding.values.push(Some(selected.to_owned()));
        add_combo_string(binding.window, "Unavailable selected endpoint");
        selected_index = binding.values.len() - 1;
    }
    unsafe {
        let _ = SendMessageW(
            binding.window,
            CB_SETCURSEL,
            Some(WPARAM(selected_index)),
            Some(LPARAM(0)),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn combo(
    parent: HWND,
    instance: HINSTANCE,
    id: usize,
    x: i32,
    y: i32,
    width: i32,
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
        width,
        260,
    )?;
    let mut binding = ComboBinding {
        window,
        values: Vec::new(),
    };
    repopulate_combo(&mut binding, endpoints, flow, selected);
    Ok(binding)
}

fn selection(binding: &ComboBinding) -> Option<String> {
    let index = unsafe { SendMessageW(binding.window, CB_GETCURSEL, None, None) }.0;
    usize::try_from(index)
        .ok()
        .and_then(|index| binding.values.get(index))
        .cloned()
        .flatten()
}

fn label(
    parent: HWND,
    instance: HINSTANCE,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    value: &str,
) -> Result<HWND> {
    let value = wide_null(value);
    control(
        parent,
        instance,
        w!("STATIC"),
        PCWSTR(value.as_ptr()),
        WS_CHILD | WS_VISIBLE,
        0,
        x,
        y,
        width,
        height,
    )
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
        28,
    )
}

#[allow(clippy::too_many_arguments)]
fn checkbox(
    parent: HWND,
    instance: HINSTANCE,
    id: usize,
    x: i32,
    y: i32,
    width: i32,
    value: &str,
    enabled: bool,
) -> Result<HWND> {
    let value = wide_null(value);
    let window = control(
        parent,
        instance,
        w!("BUTTON"),
        PCWSTR(value.as_ptr()),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
        id,
        x,
        y,
        width,
        30,
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

#[allow(clippy::too_many_arguments)]
fn button(
    parent: HWND,
    instance: HINSTANCE,
    id: usize,
    x: i32,
    y: i32,
    width: i32,
    value: &str,
    primary: bool,
) -> Result<HWND> {
    let value = wide_null(value);
    let _ = primary;
    let style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32);
    control(
        parent,
        instance,
        w!("BUTTON"),
        PCWSTR(value.as_ptr()),
        style,
        id,
        x,
        y,
        width,
        34,
    )
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
    .context("creating setup control")
}

fn open_driver_download(window: HWND) {
    let target = wide_null(DRIVER_DOWNLOAD_URL);
    let result = unsafe {
        ShellExecuteW(
            Some(window),
            w!("open"),
            PCWSTR(target.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        show_error(
            window,
            "Windows could not open the virtual cable download page.",
        );
    }
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

fn set_text(window: HWND, value: &str) {
    let value = wide_null(value);
    unsafe {
        let _ = SetWindowTextW(window, PCWSTR(value.as_ptr()));
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
            w!("Audio Switch setup"),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(id: &str, name: &str, flow: EndpointFlow, available: bool) -> Endpoint {
        Endpoint {
            id: id.into(),
            name: name.into(),
            flow,
            available,
        }
    }

    fn configured() -> AppConfig {
        let mut config = AppConfig::default();
        config.endpoints.primary_render = Some("primary-render".into());
        config.endpoints.secondary_render = Some("secondary-render".into());
        config.endpoints.primary_capture = Some("primary-capture".into());
        config.endpoints.secondary_capture = Some("secondary-capture".into());
        config.endpoints.virtual_render = Some("virtual-render".into());
        config
    }

    fn available_endpoints() -> Vec<Endpoint> {
        vec![
            endpoint("primary-render", "Speakers", EndpointFlow::Render, true),
            endpoint("secondary-render", "Headset", EndpointFlow::Render, true),
            endpoint("primary-capture", "Desk mic", EndpointFlow::Capture, true),
            endpoint(
                "secondary-capture",
                "Headset mic",
                EndpointFlow::Capture,
                true,
            ),
            endpoint(
                "virtual-render",
                "CABLE Input (VB-Audio Virtual Cable)",
                EndpointFlow::Render,
                true,
            ),
        ]
    }

    #[test]
    fn recognizes_supported_cable_without_coupling_routing_to_its_name() {
        let cable = endpoint(
            "arbitrary-endpoint-id",
            "CABLE Input (VB-Audio Virtual Cable)",
            EndpointFlow::Render,
            true,
        );
        assert!(recognized_virtual_cable(&cable));

        let physical = endpoint("speaker-id", "USB Speakers", EndpointFlow::Render, true);
        assert!(!recognized_virtual_cable(&physical));
    }

    #[test]
    fn standard_cable_input_wins_over_the_16_channel_endpoint() {
        let endpoints = [
            endpoint(
                "cable-16ch",
                "CABLE In 16ch (VB-Audio Virtual Cable)",
                EndpointFlow::Render,
                true,
            ),
            endpoint(
                "cable-input",
                "CABLE Input (VB-Audio Virtual Cable)",
                EndpointFlow::Render,
                true,
            ),
        ];

        assert_eq!(
            preferred_virtual_cable(&endpoints).map(|endpoint| endpoint.id.as_str()),
            Some("cable-input")
        );
    }

    #[test]
    fn missing_virtual_cable_is_detected_by_endpoint_id() {
        let config = configured();
        assert!(!virtual_cable_missing(&config, &available_endpoints()));
        assert!(!virtual_cable_missing(
            &AppConfig::default(),
            &available_endpoints()
        ));

        let endpoints = available_endpoints()
            .into_iter()
            .filter(|endpoint| endpoint.id != "virtual-render")
            .collect::<Vec<_>>();
        assert!(virtual_cable_missing(&config, &endpoints));
    }

    #[test]
    fn validation_requires_all_selected_endpoints_to_be_online() {
        let config = configured();
        assert!(validate_configuration(&config, &available_endpoints()).is_ok());

        let mut endpoints = available_endpoints();
        endpoints
            .iter_mut()
            .find(|endpoint| endpoint.id == "secondary-capture")
            .unwrap()
            .available = false;
        assert_eq!(
            validate_configuration(&config, &endpoints).unwrap_err(),
            "Secondary microphone is missing or unavailable."
        );
    }
}

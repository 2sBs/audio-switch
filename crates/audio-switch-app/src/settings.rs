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
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::DRAWITEMSTRUCT;
use windows::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_OWNERDRAW, CB_ADDSTRING, CB_GETCURSEL,
    CB_SETCURSEL, CBS_DROPDOWNLIST, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow,
    ES_AUTOHSCROLL, GetClientRect, GetWindowTextLengthW, GetWindowTextW, HMENU, IDYES,
    MB_DEFBUTTON2, MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_YESNO, MessageBoxW, RegisterClassW,
    SW_HIDE, SW_SHOW, SendMessageW, SetForegroundWindow, SetWindowTextW, ShowWindow,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CTLCOLORBTN, WM_CTLCOLOREDIT,
    WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DRAWITEM, WM_PAINT, WNDCLASSW, WS_BORDER,
    WS_CAPTION, WS_CHILD, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};
use windows::core::{PCWSTR, w};

use crate::ui::{self, ButtonStyle, Theme};

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
const ID_RESET: usize = 402;
const ID_GENERAL: usize = 500;
const ID_UPDATES: usize = 501;
const ID_ABOUT: usize = 502;

type SaveCallback = fn(AppConfig) -> Result<()>;
type ResetCallback = fn(HWND) -> Result<()>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SettingsPage {
    General,
    Updates,
    About,
}

struct ComboBinding {
    window: HWND,
    values: Vec<Option<String>>,
}

struct SettingsContext {
    window: HWND,
    owner: HWND,
    page: SettingsPage,
    rail_heading: HWND,
    heading: HWND,
    description: HWND,
    page_body: HWND,
    general_controls: Vec<HWND>,
    muted_labels: Vec<HWND>,
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
    save_button: HWND,
    cancel_button: HWND,
    config: AppConfig,
    save: SaveCallback,
    reset: ResetCallback,
    theme: Theme,
}

// All settings-window access occurs on the tray's Windows message thread.
unsafe impl Send for SettingsContext {}

static SETTINGS: OnceLock<Mutex<Option<SettingsContext>>> = OnceLock::new();

pub fn open(
    owner: HWND,
    config: AppConfig,
    endpoints: Vec<Endpoint>,
    save: SaveCallback,
    reset: ResetCallback,
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
    let black_brush = HBRUSH(unsafe { GetStockObject(BLACK_BRUSH) }.0);
    let class = WNDCLASSW {
        hInstance: instance,
        lpszClassName: w!("AudioSwitch.SettingsWindow.v1"),
        lpfnWndProc: Some(window_proc),
        hbrBackground: black_brush,
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
            760,
            650,
            Some(owner),
            None,
            Some(instance),
            None,
        )
    }
    .context("creating settings window")?;
    ui::use_dark_title_bar(window);
    let theme = Theme::new()?;

    let rail_heading = label(window, instance, 22, 27, 110, 28, "Settings")?;
    let general_tab = button(window, instance, ID_GENERAL, 12, 74, 126, "General")?;
    let updates_tab = button(window, instance, ID_UPDATES, 12, 110, 126, "Updates")?;
    let about_tab = button(window, instance, ID_ABOUT, 12, 146, 126, "About")?;

    let heading = label(window, instance, 180, 25, 540, 34, "General")?;
    let description = label(
        window,
        instance,
        180,
        61,
        540,
        26,
        "Configure device roles and startup behavior.",
    )?;
    let page_body = label(window, instance, 180, 112, 540, 160, "")?;
    unsafe {
        let _ = ShowWindow(page_body, SW_HIDE);
    }

    let mut general_controls = Vec::new();
    let mut muted_labels = vec![description, page_body];

    let device_roles = label(window, instance, 180, 103, 130, 22, "DEVICE ROLES")?;
    let primary_column = label(window, instance, 272, 128, 195, 22, "Primary")?;
    let secondary_column = label(window, instance, 482, 128, 238, 22, "Secondary")?;
    let name_label = label(window, instance, 180, 157, 76, 22, "Name")?;
    let playback_label = label(window, instance, 180, 197, 76, 22, "Playback")?;
    let microphone_label = label(window, instance, 180, 237, 76, 22, "Microphone")?;
    muted_labels.extend([device_roles, name_label, playback_label, microphone_label]);

    let primary_name = edit(
        window,
        instance,
        ID_PRIMARY_NAME,
        272,
        151,
        196,
        &config.labels.primary,
    )?;
    let secondary_name = edit(
        window,
        instance,
        ID_SECONDARY_NAME,
        482,
        151,
        238,
        &config.labels.secondary,
    )?;
    let primary_render = combo(
        window,
        instance,
        ID_PRIMARY_RENDER,
        272,
        191,
        196,
        &endpoints,
        EndpointFlow::Render,
        config.endpoints.primary_render.as_deref(),
    )?;
    let secondary_render = combo(
        window,
        instance,
        ID_SECONDARY_RENDER,
        482,
        191,
        238,
        &endpoints,
        EndpointFlow::Render,
        config.endpoints.secondary_render.as_deref(),
    )?;
    let primary_capture = combo(
        window,
        instance,
        ID_PRIMARY_CAPTURE,
        272,
        231,
        196,
        &endpoints,
        EndpointFlow::Capture,
        config.endpoints.primary_capture.as_deref(),
    )?;
    let secondary_capture = combo(
        window,
        instance,
        ID_SECONDARY_CAPTURE,
        482,
        231,
        238,
        &endpoints,
        EndpointFlow::Capture,
        config.endpoints.secondary_capture.as_deref(),
    )?;

    let virtual_heading = label(window, instance, 180, 281, 180, 22, "VIRTUAL MICROPHONE")?;
    muted_labels.push(virtual_heading);
    let virtual_render = combo(
        window,
        instance,
        ID_VIRTUAL_RENDER,
        180,
        305,
        540,
        &endpoints,
        EndpointFlow::Render,
        config.endpoints.virtual_render.as_deref(),
    )?;

    let behavior_heading = label(window, instance, 180, 355, 120, 22, "BEHAVIOR")?;
    muted_labels.push(behavior_heading);
    let cue_enabled = checkbox(
        window,
        instance,
        ID_CUES,
        180,
        380,
        260,
        "Play a cue when switching",
        config.audio.cue_enabled,
    )?;
    let auto_start = checkbox(
        window,
        instance,
        ID_AUTOSTART,
        180,
        410,
        260,
        "Start at Windows sign-in",
        config.auto_start,
    )?;
    let auto_start_elevated = checkbox(
        window,
        instance,
        ID_AUTOSTART_ELEVATED,
        180,
        440,
        450,
        "Request UAC elevation and use high process priority",
        config.auto_start_elevated,
    )?;

    let reset_heading = label(
        window,
        instance,
        180,
        492,
        260,
        22,
        "Reset settings and run setup",
    )?;
    let reset_description = label(
        window,
        instance,
        180,
        514,
        360,
        36,
        "Starts setup with fresh defaults. Nothing changes unless setup finishes.",
    )?;
    muted_labels.push(reset_description);
    let reset_button = button(window, instance, ID_RESET, 594, 501, 126, "Reset...")?;
    let save_button = button(window, instance, ID_SAVE, 590, 567, 130, "Save changes")?;
    let cancel_button = button(window, instance, ID_CANCEL, 498, 567, 82, "Cancel")?;

    general_controls.extend([
        device_roles,
        primary_column,
        secondary_column,
        name_label,
        playback_label,
        microphone_label,
        primary_name,
        secondary_name,
        primary_render.window,
        secondary_render.window,
        primary_capture.window,
        secondary_capture.window,
        virtual_heading,
        virtual_render.window,
        behavior_heading,
        cue_enabled,
        auto_start,
        auto_start_elevated,
        reset_heading,
        reset_description,
        reset_button,
    ]);

    theme.set_semibold(rail_heading);
    theme.set_heading(heading);
    theme.set_small(description);
    theme.set_body(primary_column);
    theme.set_body(secondary_column);
    theme.set_semibold(reset_heading);
    for label in &muted_labels {
        theme.set_small(*label);
    }
    for control in [
        general_tab,
        updates_tab,
        about_tab,
        primary_name,
        secondary_name,
        primary_render.window,
        secondary_render.window,
        primary_capture.window,
        secondary_capture.window,
        virtual_render.window,
        cue_enabled,
        auto_start,
        auto_start_elevated,
        reset_button,
        save_button,
        cancel_button,
    ] {
        theme.set_body(control);
        ui::use_dark_control(control);
    }

    *storage.lock().expect("settings mutex poisoned") = Some(SettingsContext {
        window,
        owner,
        page: SettingsPage::General,
        rail_heading,
        heading,
        description,
        page_body,
        general_controls,
        muted_labels,
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
        save_button,
        cancel_button,
        config,
        save,
        reset,
        theme,
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
                ID_GENERAL => switch_page(SettingsPage::General),
                ID_UPDATES => switch_page(SettingsPage::Updates),
                ID_ABOUT => switch_page(SettingsPage::About),
                ID_SAVE => {
                    if current_page() == Some(SettingsPage::General) {
                        match save_settings() {
                            Ok(()) => unsafe {
                                let _ = DestroyWindow(window);
                            },
                            Err(error) => show_error(window, &error.to_string()),
                        }
                    } else {
                        unsafe {
                            let _ = DestroyWindow(window);
                        }
                    }
                }
                ID_CANCEL => unsafe {
                    let _ = DestroyWindow(window);
                },
                ID_RESET => reset_settings(window),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DRAWITEM => {
            let item = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
            if let Some(storage) = SETTINGS.get()
                && let Some(context) = storage.lock().expect("settings mutex poisoned").as_ref()
            {
                let style = match item.CtlID as usize {
                    ID_SAVE => ButtonStyle::Primary,
                    ID_RESET => ButtonStyle::Danger,
                    ID_GENERAL if context.page == SettingsPage::General => ButtonStyle::NavActive,
                    ID_UPDATES if context.page == SettingsPage::Updates => ButtonStyle::NavActive,
                    ID_ABOUT if context.page == SettingsPage::About => ButtonStyle::NavActive,
                    ID_GENERAL | ID_UPDATES | ID_ABOUT => ButtonStyle::Nav,
                    _ => ButtonStyle::Standard,
                };
                ui::draw_button(item, &context.theme, style);
                return LRESULT(1);
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let device_context = HDC(wparam.0 as *mut c_void);
            let control = HWND(lparam.0 as *mut c_void);
            if let Some(storage) = SETTINGS.get()
                && let Some(context) = storage.lock().expect("settings mutex poisoned").as_ref()
            {
                let text = if context.muted_labels.contains(&control) {
                    ui::MUTED
                } else {
                    ui::TEXT
                };
                let rail_control = control == context.rail_heading;
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
            if let Some(storage) = SETTINGS.get()
                && let Some(context) = storage.lock().expect("settings mutex poisoned").as_ref()
            {
                let mut client = RECT::default();
                unsafe {
                    let _ = GetClientRect(window, &mut client);
                    let rail = RECT {
                        left: 0,
                        top: 0,
                        right: 151,
                        bottom: client.bottom,
                    };
                    let divider = RECT {
                        left: 150,
                        top: 0,
                        right: 151,
                        bottom: client.bottom,
                    };
                    let _ = FillRect(device_context, &rail, context.theme.panel);
                    let _ = FillRect(device_context, &divider, context.theme.line);
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
            if let Some(storage) = SETTINGS.get() {
                *storage.lock().expect("settings mutex poisoned") = None;
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

fn current_page() -> Option<SettingsPage> {
    SETTINGS
        .get()
        .and_then(|storage| storage.lock().ok()?.as_ref().map(|context| context.page))
}

fn switch_page(page: SettingsPage) {
    let Some(storage) = SETTINGS.get() else {
        return;
    };
    let (
        heading_control,
        description_control,
        page_body,
        general_controls,
        cancel_button,
        save_button,
        window,
    ) = {
        let mut storage = storage.lock().expect("settings mutex poisoned");
        let Some(context) = storage.as_mut() else {
            return;
        };
        context.page = page;
        (
            context.heading,
            context.description,
            context.page_body,
            context.general_controls.clone(),
            context.cancel_button,
            context.save_button,
            context.window,
        )
    };
    let (heading, description, body) = page_copy(page);
    set_text(heading_control, heading);
    set_text(description_control, description);
    set_text(page_body, &body);
    let general = page == SettingsPage::General;
    for control in general_controls {
        unsafe {
            let _ = ShowWindow(control, if general { SW_SHOW } else { SW_HIDE });
        }
    }
    unsafe {
        let _ = ShowWindow(page_body, if general { SW_HIDE } else { SW_SHOW });
        let _ = ShowWindow(cancel_button, if general { SW_SHOW } else { SW_HIDE });
    }
    set_text(save_button, if general { "Save changes" } else { "Close" });
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(window), None, true);
    }
}

fn page_copy(page: SettingsPage) -> (&'static str, &'static str, String) {
    match page {
        SettingsPage::General => (
            "General",
            "Configure device roles and startup behavior.",
            String::new(),
        ),
        SettingsPage::Updates => (
            "Updates",
            "Manage how Audio Switch receives new versions.",
            "Update controls are not available in this release.".into(),
        ),
        SettingsPage::About => (
            "About",
            "Version and application information.",
            format!(
                "Audio Switch {}\r\n\r\nNative Windows audio routing utility.",
                env!("CARGO_PKG_VERSION")
            ),
        ),
    }
}

fn reset_settings(window: HWND) {
    let confirmed = unsafe {
        MessageBoxW(
            Some(window),
            w!(
                "Setup will start with fresh defaults. Your current settings and audio routing stay active unless you finish setup.\r\n\r\nReset settings and run setup?"
            ),
            w!("Reset Audio Switch"),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        ) == IDYES
    };
    if !confirmed {
        return;
    }
    let callback = SETTINGS.get().and_then(|storage| {
        let context = storage.lock().ok()?;
        let context = context.as_ref()?;
        Some((context.owner, context.reset))
    });
    let Some((owner, reset)) = callback else {
        show_error(window, "Settings are no longer available.");
        return;
    };
    unsafe {
        let _ = DestroyWindow(window);
    }
    if let Err(error) = reset(owner) {
        show_error(owner, &format!("Could not reset Audio Switch: {error:#}"));
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
        28,
    )
}

fn label(
    parent: HWND,
    instance: HINSTANCE,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    text: &str,
) -> Result<HWND> {
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
        height,
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
        width,
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
) -> Result<HWND> {
    let text = wide_null(text);
    control(
        parent,
        instance,
        w!("BUTTON"),
        PCWSTR(text.as_ptr()),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
        id,
        x,
        y,
        width,
        32,
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
            w!("Audio Switch settings"),
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

    #[test]
    fn future_settings_pages_are_explicit_placeholders() {
        assert!(page_copy(SettingsPage::Updates).2.contains("not available"));
        assert!(
            page_copy(SettingsPage::About)
                .2
                .contains(env!("CARGO_PKG_VERSION"))
        );
    }
}

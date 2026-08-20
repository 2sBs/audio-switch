use std::ffi::c_void;
use std::mem::size_of;

use anyhow::Result;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET,
    DEFAULT_PITCH, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, FF_DONTCARE,
    FW_NORMAL, FW_SEMIBOLD, FillRect, FrameRect, HBRUSH, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, ODS_DISABLED, ODS_FOCUS, ODS_SELECTED, SetWindowTheme,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowTextLengthW, GetWindowTextW, SendMessageW, WM_SETFONT,
};
use windows::core::{PCWSTR, w};

pub const BACKGROUND: COLORREF = COLORREF(0x0000_0000);
pub const PANEL: COLORREF = COLORREF(0x0005_0505);
pub const CONTROL: COLORREF = COLORREF(0x0011_1111);
pub const TEXT: COLORREF = COLORREF(0x00ff_ffff);
pub const MUTED: COLORREF = COLORREF(0x0092_9292);
pub const QUIET: COLORREF = COLORREF(0x0069_6969);
pub const ACCENT: COLORREF = COLORREF(0x00b6_a5ff);
pub const SUCCESS: COLORREF = COLORREF(0x0091_d369);
pub const DANGER: COLORREF = COLORREF(0x0087_7fff);

pub struct Theme {
    pub background: HBRUSH,
    pub panel: HBRUSH,
    pub control: HBRUSH,
    pub line: HBRUSH,
    pub accent: HBRUSH,
    pub danger: HBRUSH,
    pub body: HFONT,
    pub body_semibold: HFONT,
    pub heading: HFONT,
    pub small: HFONT,
}

impl Theme {
    pub fn new() -> Result<Self> {
        let theme = Self {
            background: unsafe { CreateSolidBrush(BACKGROUND) },
            panel: unsafe { CreateSolidBrush(PANEL) },
            control: unsafe { CreateSolidBrush(CONTROL) },
            line: unsafe { CreateSolidBrush(COLORREF(0x0029_2929)) },
            accent: unsafe { CreateSolidBrush(ACCENT) },
            danger: unsafe { CreateSolidBrush(DANGER) },
            body: font(15, false),
            body_semibold: font(15, true),
            heading: font(23, true),
            small: font(13, false),
        };
        if theme.background.is_invalid()
            || theme.panel.is_invalid()
            || theme.control.is_invalid()
            || theme.line.is_invalid()
            || theme.accent.is_invalid()
            || theme.danger.is_invalid()
            || theme.body.is_invalid()
            || theme.body_semibold.is_invalid()
            || theme.heading.is_invalid()
            || theme.small.is_invalid()
        {
            anyhow::bail!("creating UI theme resources failed");
        }
        Ok(theme)
    }

    pub fn set_body(&self, window: HWND) {
        set_font(window, self.body);
    }

    pub fn set_semibold(&self, window: HWND) {
        set_font(window, self.body_semibold);
    }

    pub fn set_heading(&self, window: HWND) {
        set_font(window, self.heading);
    }

    pub fn set_small(&self, window: HWND) {
        set_font(window, self.small);
    }
}

impl Drop for Theme {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.background.0));
            let _ = DeleteObject(HGDIOBJ(self.panel.0));
            let _ = DeleteObject(HGDIOBJ(self.control.0));
            let _ = DeleteObject(HGDIOBJ(self.line.0));
            let _ = DeleteObject(HGDIOBJ(self.accent.0));
            let _ = DeleteObject(HGDIOBJ(self.danger.0));
            let _ = DeleteObject(HGDIOBJ(self.body.0));
            let _ = DeleteObject(HGDIOBJ(self.body_semibold.0));
            let _ = DeleteObject(HGDIOBJ(self.heading.0));
            let _ = DeleteObject(HGDIOBJ(self.small.0));
        }
    }
}

#[derive(Clone, Copy)]
pub enum ButtonStyle {
    Standard,
    Primary,
    Danger,
    Nav,
    NavActive,
}

pub fn draw_button(item: &DRAWITEMSTRUCT, theme: &Theme, style: ButtonStyle) {
    let disabled = item.itemState.0 & ODS_DISABLED.0 != 0;
    let selected = item.itemState.0 & ODS_SELECTED.0 != 0;
    let (brush, text) = match style {
        ButtonStyle::Primary => (theme.accent, BACKGROUND),
        ButtonStyle::Danger => (theme.background, DANGER),
        ButtonStyle::Nav => (theme.panel, QUIET),
        ButtonStyle::NavActive => (theme.control, ACCENT),
        ButtonStyle::Standard => (theme.background, TEXT),
    };
    let brush = if selected && !matches!(style, ButtonStyle::Primary) {
        theme.control
    } else {
        brush
    };
    let text = if disabled { QUIET } else { text };
    let mut rect = item.rcItem;
    let mut value = window_text(item.hwndItem);
    unsafe {
        let _ = FillRect(item.hDC, &rect, brush);
        let border = if matches!(style, ButtonStyle::Primary | ButtonStyle::NavActive)
            || item.itemState.0 & ODS_FOCUS.0 != 0
        {
            theme.accent
        } else {
            theme.line
        };
        let _ = FrameRect(item.hDC, &rect, border);
        let previous = SelectObject(item.hDC, HGDIOBJ(theme.body_semibold.0));
        let _ = SetBkMode(item.hDC, TRANSPARENT);
        let _ = SetTextColor(item.hDC, text);
        let _ = DrawTextW(
            item.hDC,
            &mut value,
            &raw mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        let _ = SelectObject(item.hDC, previous);
    }
}

pub fn use_dark_title_bar(window: HWND) {
    let enabled = 1_i32;
    unsafe {
        let _ = DwmSetWindowAttribute(
            window,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&raw const enabled).cast::<c_void>(),
            size_of::<i32>() as u32,
        );
    }
}

pub fn use_dark_control(window: HWND) {
    unsafe {
        let _ = SetWindowTheme(window, w!("DarkMode_Explorer"), PCWSTR::null());
    }
}

fn font(height: i32, semibold: bool) -> HFONT {
    unsafe {
        CreateFontW(
            -height,
            0,
            0,
            0,
            if semibold {
                FW_SEMIBOLD.0 as i32
            } else {
                FW_NORMAL.0 as i32
            },
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH.0 as u32 | FF_DONTCARE.0 as u32,
            w!("Segoe UI Variable Text"),
        )
    }
}

fn set_font(window: HWND, font: HFONT) {
    unsafe {
        let _ = SendMessageW(
            window,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
    }
}

fn window_text(window: HWND) -> Vec<u16> {
    let length = unsafe { GetWindowTextLengthW(window) }.max(0) as usize;
    let mut value = vec![0_u16; length + 1];
    let copied = unsafe { GetWindowTextW(window, &mut value) }.max(0) as usize;
    value.truncate(copied);
    value
}

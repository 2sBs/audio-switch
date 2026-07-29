use std::path::Path;

use anyhow::{Context, Result};
#[cfg(not(debug_assertions))]
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteValueW, RegSetValueExW,
};
use windows::Win32::System::Threading::{GetCurrentProcess, HIGH_PRIORITY_CLASS, SetPriorityClass};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{PCWSTR, w};

#[cfg(not(debug_assertions))]
const VALUE_NAME: PCWSTR = w!("Audio Switch");

#[cfg(not(debug_assertions))]
pub fn reconcile(enabled: bool, elevated: bool, executable: &Path) -> Result<()> {
    let mut key = HKEY::default();
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Run"),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
        .ok()
    }
    .context("opening the current-user startup settings")?;
    let result = if enabled {
        let command = startup_command(executable, elevated);
        let wide: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
        let bytes = unsafe {
            std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * size_of::<u16>())
        };
        unsafe { RegSetValueExW(key, VALUE_NAME, None, REG_SZ, Some(bytes)).ok() }
            .context("enabling Audio Switch at sign-in")
    } else {
        let status = unsafe { RegDeleteValueW(key, VALUE_NAME) };
        if status.0 == 2 {
            Ok(())
        } else {
            status.ok().context("disabling Audio Switch at sign-in")
        }
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    result
}

pub fn launch_elevated(executable: &Path) -> Result<()> {
    let executable = wide_null(&executable.to_string_lossy());
    let parameters = wide_null("--tray --high-priority");
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("runas"),
            PCWSTR(executable.as_ptr()),
            PCWSTR(parameters.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        anyhow::bail!(
            "requesting elevation for Audio Switch failed with shell error {}",
            result.0 as isize
        );
    }
    Ok(())
}

pub fn set_high_process_priority() -> Result<()> {
    unsafe { SetPriorityClass(GetCurrentProcess(), HIGH_PRIORITY_CLASS) }
        .context("setting Audio Switch to high process priority")
}

#[cfg(any(not(debug_assertions), test))]
fn startup_command(executable: &Path, elevated: bool) -> String {
    let argument = if elevated {
        "--elevated-autostart"
    } else {
        "--tray"
    };
    format!("\"{}\" {argument}", executable.display())
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_startup_command_keeps_the_existing_tray_path() {
        let executable = Path::new(r"C:\Program Files\Audio Switch\audio-switch.exe");
        assert_eq!(
            startup_command(executable, false),
            r#""C:\Program Files\Audio Switch\audio-switch.exe" --tray"#
        );
    }

    #[test]
    fn elevated_startup_command_uses_the_elevation_launcher() {
        let executable = Path::new(r"C:\Program Files\Audio Switch\audio-switch.exe");
        assert_eq!(
            startup_command(executable, true),
            r#""C:\Program Files\Audio Switch\audio-switch.exe" --elevated-autostart"#
        );
    }
}

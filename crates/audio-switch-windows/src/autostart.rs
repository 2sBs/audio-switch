use std::path::Path;

use anyhow::{Context, Result};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
};
#[cfg(not(debug_assertions))]
use windows::Win32::System::Registry::{
    KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCreateKeyExW, RegDeleteValueW,
    RegSetValueExW,
};
use windows::Win32::System::Threading::{GetCurrentProcess, HIGH_PRIORITY_CLASS, SetPriorityClass};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{PCWSTR, w};

const VALUE_NAME: PCWSTR = w!("Audio Switch");
const STARTUP_APPROVED_KEY: PCWSTR =
    w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StartupApproval {
    Enabled,
    Disabled,
    Unknown,
}

#[cfg(any(not(debug_assertions), test))]
#[derive(Debug, Eq, PartialEq)]
enum ReconcileAction {
    Keep,
    Set(String),
    Delete,
}

fn startup_approval(value: &[u8]) -> StartupApproval {
    match value.first() {
        Some(2) => StartupApproval::Enabled,
        Some(3) => StartupApproval::Disabled,
        _ => StartupApproval::Unknown,
    }
}

#[cfg(any(not(debug_assertions), test))]
fn reconcile_action(
    enabled: bool,
    elevated: bool,
    executable: &Path,
    current: Option<&str>,
) -> ReconcileAction {
    if !enabled {
        return if current.is_some() {
            ReconcileAction::Delete
        } else {
            ReconcileAction::Keep
        };
    }
    let expected = startup_command(executable, elevated);
    if current == Some(expected.as_str()) {
        ReconcileAction::Keep
    } else {
        ReconcileAction::Set(expected)
    }
}

pub fn disabled_by_windows() -> Result<bool> {
    let mut key = HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            STARTUP_APPROVED_KEY,
            None,
            KEY_QUERY_VALUE,
            &mut key,
        )
    };
    if status.0 == 2 {
        return Ok(false);
    }
    status
        .ok()
        .context("opening the Windows startup approval settings")?;

    let mut value = [0_u8; 12];
    let mut size = value.len() as u32;
    let status = unsafe {
        RegQueryValueExW(
            key,
            VALUE_NAME,
            None,
            None,
            Some(value.as_mut_ptr()),
            Some(&mut size),
        )
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    if status.0 == 2 {
        return Ok(false);
    }
    status
        .ok()
        .context("reading the Windows startup approval settings")?;
    Ok(startup_approval(&value[..size as usize]) == StartupApproval::Disabled)
}

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
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
        .ok()
    }
    .context("opening the current-user startup settings")?;
    let current = read_run_value(key);
    let result = current.and_then(|current| {
        match reconcile_action(enabled, elevated, executable, current.as_deref()) {
            ReconcileAction::Keep => Ok(()),
            ReconcileAction::Set(command) => {
                let wide: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        wide.as_ptr().cast::<u8>(),
                        wide.len() * size_of::<u16>(),
                    )
                };
                unsafe { RegSetValueExW(key, VALUE_NAME, None, REG_SZ, Some(bytes)).ok() }
                    .context("enabling Audio Switch at sign-in")
            }
            ReconcileAction::Delete => unsafe { RegDeleteValueW(key, VALUE_NAME) }
                .ok()
                .context("disabling Audio Switch at sign-in"),
        }
    });
    unsafe {
        let _ = RegCloseKey(key);
    }
    result
}

#[cfg(not(debug_assertions))]
fn read_run_value(key: HKEY) -> Result<Option<String>> {
    let mut value_type = windows::Win32::System::Registry::REG_VALUE_TYPE::default();
    let mut value = [0_u16; 1024];
    let mut size = (value.len() * size_of::<u16>()) as u32;
    let status = unsafe {
        RegQueryValueExW(
            key,
            VALUE_NAME,
            None,
            Some(&mut value_type),
            Some(value.as_mut_ptr().cast::<u8>()),
            Some(&mut size),
        )
    };
    if status.0 == 2 {
        return Ok(None);
    }
    status
        .ok()
        .context("reading the Audio Switch startup command")?;
    if value_type != REG_SZ || size as usize % size_of::<u16>() != 0 {
        anyhow::bail!("the Audio Switch startup command has an invalid registry type");
    }
    let mut length = size as usize / size_of::<u16>();
    if length > 0 && value[length - 1] == 0 {
        length -= 1;
    }
    Ok(Some(
        String::from_utf16(&value[..length])
            .context("decoding the Audio Switch startup command")?,
    ))
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
    fn windows_disabled_startup_is_reported_as_blocked() {
        assert_eq!(
            startup_approval(&[3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            StartupApproval::Disabled
        );
    }

    #[test]
    fn unchanged_startup_command_is_not_rewritten() {
        let executable = Path::new(r"C:\Program Files\Audio Switch\audio-switch.exe");
        let current = r#""C:\Program Files\Audio Switch\audio-switch.exe" --tray"#;

        assert_eq!(
            reconcile_action(true, false, executable, Some(current)),
            ReconcileAction::Keep
        );
    }

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

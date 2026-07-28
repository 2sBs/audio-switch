use std::path::Path;

use anyhow::{Context, Result};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteValueW, RegSetValueExW,
};
use windows::core::{PCWSTR, w};

const VALUE_NAME: PCWSTR = w!("Audio Switch");

pub fn reconcile(enabled: bool, executable: &Path) -> Result<()> {
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
        let command = format!("\"{}\" --tray", executable.display());
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

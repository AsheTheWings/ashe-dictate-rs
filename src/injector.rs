use crate::logger;
use crate::util::wide;
use anyhow::{Context, Result, anyhow};
use std::mem::size_of;
use std::thread;
use std::time::Duration;
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VK_CONTROL,
};
use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;

const CLIPBOARD_RETRIES: usize = 12;
const CLIPBOARD_RETRY_DELAY: Duration = Duration::from_millis(25);
const PASTE_SETTLE_DELAY: Duration = Duration::from_millis(180);

pub fn copy_text(text: &str) -> Result<()> {
    set_clipboard_text(text).context("failed to set clipboard text")?;
    logger::info("Copied text to clipboard");
    Ok(())
}

pub fn paste_text(text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }

    let previous_text = read_clipboard_text().ok().flatten();
    set_clipboard_text(text).context("failed to set clipboard text")?;
    logger::info(format!("Pasting text: {text}"));
    send_ctrl_v().context("failed to send Ctrl+V")?;
    thread::sleep(PASTE_SETTLE_DELAY);

    if let Some(previous_text) = previous_text {
        if let Err(err) = set_clipboard_text(&previous_text) {
            logger::info(format!("Clipboard restore failed: {err:#}"));
        }
    }

    Ok(())
}

pub fn paste_text_to(hwnd: HWND, text: &str) -> Result<()> {
    unsafe {
        if !hwnd.0.is_null() {
            let _ = SetForegroundWindow(hwnd);
            thread::sleep(Duration::from_millis(90));
        }
    }
    paste_text(text)
}

fn read_clipboard_text() -> Result<Option<String>> {
    let _guard = ClipboardGuard::open()?;
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).is_err() {
            return Ok(None);
        }

        let handle = GetClipboardData(CF_UNICODETEXT.0 as u32)?;
        let global = HGLOBAL(handle.0);
        let size = GlobalSize(global) / size_of::<u16>();
        if size == 0 {
            return Ok(None);
        }

        let ptr = GlobalLock(global) as *const u16;
        if ptr.is_null() {
            return Err(anyhow!("GlobalLock failed while reading clipboard"));
        }

        let slice = std::slice::from_raw_parts(ptr, size);
        let end = slice
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(slice.len());
        let text = String::from_utf16_lossy(&slice[..end]);
        let _ = GlobalUnlock(global);
        Ok(Some(text))
    }
}

fn set_clipboard_text(text: &str) -> Result<()> {
    let _guard = ClipboardGuard::open()?;
    unsafe {
        EmptyClipboard()?;
        let wide_text = wide(text);
        let bytes = wide_text.len() * size_of::<u16>();
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes)?;
        let ptr = GlobalLock(memory) as *mut u16;
        if ptr.is_null() {
            return Err(anyhow!("GlobalLock failed while setting clipboard"));
        }

        ptr.copy_from_nonoverlapping(wide_text.as_ptr(), wide_text.len());
        let _ = GlobalUnlock(memory);
        SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(memory.0)))?;
        Ok(())
    }
}

struct ClipboardGuard;

impl ClipboardGuard {
    fn open() -> Result<Self> {
        for attempt in 1..=CLIPBOARD_RETRIES {
            unsafe {
                if OpenClipboard(None).is_ok() {
                    return Ok(Self);
                }
            }
            logger::info(format!(
                "Clipboard open retry {attempt}/{CLIPBOARD_RETRIES}"
            ));
            thread::sleep(CLIPBOARD_RETRY_DELAY);
        }
        Err(anyhow!("clipboard is busy"))
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

fn send_ctrl_v() -> Result<()> {
    unsafe {
        let mut inputs = [
            key_input(VK_CONTROL.0 as u16, false),
            key_input('V' as u16, false),
            key_input('V' as u16, true),
            key_input(VK_CONTROL.0 as u16, true),
        ];
        let sent = SendInput(&mut inputs, size_of::<INPUT>() as i32);
        if sent != inputs.len() as u32 {
            return Err(anyhow!("SendInput sent {sent}/{} events", inputs.len()));
        }
    }
    Ok(())
}

fn key_input(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    Default::default()
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

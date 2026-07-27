use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ActivitySample {
    pub ts: i64,
    pub exe: String,
    pub title: String,
    pub idle_s: f64,
    pub locked: bool,
}

impl ActivitySample {
    pub fn label(&self) -> String {
        if self.locked {
            "workstation locked".to_string()
        } else if self.title.is_empty() {
            self.exe.clone()
        } else if self.exe.is_empty() {
            self.title.clone()
        } else {
            format!("{} — {}", self.exe, self.title)
        }
    }
}

#[cfg(target_os = "windows")]
pub fn sample(ts: i64) -> ActivitySample {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::StationsAndDesktops::{
        CloseDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_SWITCHDESKTOP, OpenInputDesktop,
    };
    use windows::Win32::System::SystemInformation::GetTickCount64;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
    };
    use windows::core::PWSTR;

    let locked = unsafe {
        match OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_SWITCHDESKTOP) {
            Ok(desktop) => {
                let _ = CloseDesktop(desktop);
                false
            }
            Err(_) => true,
        }
    };
    if locked {
        return ActivitySample {
            ts,
            locked: true,
            ..Default::default()
        };
    }

    let hwnd = unsafe { GetForegroundWindow() };
    let mut title_buffer = vec![0_u16; 1024];
    let title_length = unsafe { GetWindowTextW(hwnd, &mut title_buffer) }.max(0) as usize;
    let title = String::from_utf16_lossy(&title_buffer[..title_length]);
    let mut pid = 0_u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    let exe = unsafe {
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .ok()
            .and_then(|handle| {
                let mut path_buffer = vec![0_u16; 1024];
                let mut length = path_buffer.len() as u32;
                let result = QueryFullProcessImageNameW(
                    handle,
                    PROCESS_NAME_FORMAT(0),
                    PWSTR(path_buffer.as_mut_ptr()),
                    &mut length,
                );
                let _ = CloseHandle(handle);
                result.ok().map(|_| {
                    let path = String::from_utf16_lossy(&path_buffer[..length as usize]);
                    path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string()
                })
            })
            .unwrap_or_default()
    };
    let idle_s = unsafe {
        let mut input = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if GetLastInputInfo(&mut input).as_bool() {
            (GetTickCount64() as u32).wrapping_sub(input.dwTime) as f64 / 1000.0
        } else {
            0.0
        }
    };
    ActivitySample {
        ts,
        exe,
        title,
        idle_s,
        locked,
    }
}

#[cfg(not(target_os = "windows"))]
pub fn sample(ts: i64) -> ActivitySample {
    ActivitySample {
        ts,
        ..Default::default()
    }
}

pub fn timeline(samples: &[ActivitySample], interval_s: f64, idle_threshold: f64) -> Vec<String> {
    if samples.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut start = 0;
    for index in 1..=samples.len() {
        let boundary = index == samples.len()
            || samples[index].locked != samples[start].locked
            || samples[index].exe != samples[start].exe
            || samples[index].title != samples[start].title;
        if !boundary {
            continue;
        }
        let first = &samples[start];
        let last = &samples[index - 1];
        let duration = ((last.ts - first.ts) as f64 + interval_s).max(interval_s);
        let max_idle = samples[start..index]
            .iter()
            .map(|item| item.idle_s)
            .fold(0.0_f64, f64::max);
        let mut line = format!(
            "{}-{} ({}) {}",
            clock(first.ts),
            clock(last.ts + interval_s as i64),
            human_duration(duration),
            first.label()
        );
        if max_idle >= idle_threshold {
            line.push_str(&format!(" [no input for {}]", human_duration(max_idle)));
        }
        lines.push(line);
        start = index;
    }
    lines
}

pub fn app_totals(samples: &[ActivitySample], interval_s: f64) -> String {
    app_total_values(samples, interval_s)
        .into_iter()
        .map(|(name, seconds)| format!("{name} {}", human_duration(seconds)))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn app_total_values(samples: &[ActivitySample], interval_s: f64) -> Vec<(String, f64)> {
    use std::collections::HashMap;
    let mut totals: HashMap<String, f64> = HashMap::new();
    for sample in samples {
        let name = if sample.locked {
            "workstation locked".to_string()
        } else if sample.exe.is_empty() {
            "unknown".to_string()
        } else {
            sample.exe.clone()
        };
        *totals.entry(name).or_default() += interval_s;
    }
    let mut totals: Vec<_> = totals.into_iter().collect();
    totals.sort_by(|left, right| right.1.total_cmp(&left.1));
    totals
}

pub fn clock(ts: i64) -> String {
    use chrono::{Local, TimeZone};
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|stamp| stamp.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "??:??:??".to_string())
}

pub fn human_duration(seconds: f64) -> String {
    let total = seconds.round().max(0.0) as u64;
    let hours = total / 3600;
    let minutes = total % 3600 / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

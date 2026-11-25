use anyhow::{bail, Result};

pub fn ensure_emulator_stopped(executable_name: &str) -> Result<()> {
    let pids = emulator_processes(executable_name)?;
    if pids.is_empty() {
        return Ok(());
    }
    let list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    bail!("Feche o emulador antes de iniciar ou recuperar (PID: {list}).");
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct MemoryStatus {
    pub physical_available: u64,
    pub commit_available: u64,
}

impl MemoryStatus {
    pub fn compact_label(self) -> String {
        let gib = 1024f64 * 1024f64 * 1024f64;
        format!(
            "{:.1} GiB RAM · {:.1} GiB commit",
            self.physical_available as f64 / gib,
            self.commit_available as f64 / gib
        )
    }
}

#[cfg(windows)]
pub fn emulator_processes(executable_name: &str) -> Result<Vec<u32>> {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let wanted = executable_name.trim().to_ascii_lowercase();
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        bail!("não foi possível consultar os processos do Windows");
    }
    let mut matches = Vec::new();
    let result = (|| -> Result<()> {
        let mut entry: PROCESSENTRY32W = unsafe { zeroed() };
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while ok {
            let name = wide_process_name(&entry.szExeFile).to_ascii_lowercase();
            let is_emulator = (name.starts_with("shadps4") && !name.contains("qtlauncher"))
                || (!wanted.is_empty() && name == wanted);
            if is_emulator {
                matches.push(entry.th32ProcessID);
            }
            ok = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }
        let error = unsafe { GetLastError() };
        if error != ERROR_NO_MORE_FILES && error != 0 {
            bail!("a consulta de processos foi interrompida pelo Windows ({error})");
        }
        Ok(())
    })();
    unsafe { CloseHandle(snapshot) };
    result?;
    Ok(matches)
}

/// Diz se o processo ainda está vivo. Usado para não deixar a atividade do
/// Discord pendurada quando o jogo fecha.
#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == 0 {
        return false;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code) != 0;
        let _ = CloseHandle(handle);
        ok && code == STILL_ACTIVE as u32
    }
}

#[cfg(not(windows))]
pub fn pid_alive(_pid: u32) -> bool {
    false
}

#[cfg(windows)]
fn wide_process_name(buffer: &[u16; 260]) -> String {
    let length = buffer
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..length])
}

#[cfg(not(windows))]
pub fn emulator_processes(_executable_name: &str) -> Result<Vec<u32>> {
    Ok(Vec::new())
}

#[cfg(windows)]
pub fn memory_status() -> Option<MemoryStatus> {
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        physical_total: u64,
        physical_available: u64,
        commit_limit: u64,
        commit_available: u64,
        virtual_total: u64,
        virtual_available: u64,
        extended_virtual: u64,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
    }

    let mut status = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        memory_load: 0,
        physical_total: 0,
        physical_available: 0,
        commit_limit: 0,
        commit_available: 0,
        virtual_total: 0,
        virtual_available: 0,
        extended_virtual: 0,
    };
    let success = unsafe { GlobalMemoryStatusEx(&mut status) };
    (success != 0).then_some(MemoryStatus {
        physical_available: status.physical_available,
        commit_available: status.commit_available,
    })
}

#[cfg(not(windows))]
pub fn memory_status() -> Option<MemoryStatus> {
    None
}

#[cfg(test)]
mod alive_tests {
    #[test]
    fn pid_alive_detects_current_and_invalid() {
        assert!(super::pid_alive(std::process::id()), "o próprio processo está vivo");
        assert!(!super::pid_alive(0), "PID 0 nunca é um jogo");
    }
}

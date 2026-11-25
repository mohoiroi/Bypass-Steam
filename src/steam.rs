//! Open or gracefully restart the Steam client in the current Windows session.

use crate::config;
use std::{
    env,
    ffi::OsString,
    io,
    os::windows::{
        ffi::OsStringExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
    },
    path::{Path, PathBuf},
    process::{Child, Command},
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
        Registry::{HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY, KEY_WOW64_64KEY, REG_SZ},
        RemoteDesktop::ProcessIdToSessionId,
        Threading::{
            GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
        },
    },
};

const STEAM_KEY: &str = r"Software\Valve\Steam";
const STEAM_EXE: &str = "steam.exe";
const SHUTDOWN_TIMEOUT_MS: u32 = 25_000;

#[derive(Clone, Copy)]
pub enum Request {
    Open,
    Restart,
}

pub enum Outcome {
    Opened,
    AlreadyOpen,
    Restarted,
}

pub enum Error {
    Check,
    NotInstalled,
    Shutdown,
    Timeout,
    Launch,
}

pub fn is_running() -> io::Result<bool> {
    running_pid().map(|pid| pid.is_some())
}

fn session_id(pid: u32) -> io::Result<u32> {
    let mut session = 0;
    if unsafe { ProcessIdToSessionId(pid, &mut session) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(session)
}

fn running_pid() -> io::Result<Option<u32>> {
    let session = session_id(unsafe { GetCurrentProcessId() })?;
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut valid = unsafe { Process32FirstW(snapshot.as_raw_handle(), &mut entry) };
    while valid != 0 {
        let len = STEAM_EXE.len();
        if entry.szExeFile[len] == 0
            && String::from_utf16_lossy(&entry.szExeFile[..len]).eq_ignore_ascii_case(STEAM_EXE)
            && session_id(entry.th32ProcessID).is_ok_and(|current| current == session)
        {
            return Ok(Some(entry.th32ProcessID));
        }
        valid = unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) };
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
        return Ok(None);
    }
    Err(error)
}

fn steam_executable(path: &Path) -> bool {
    path.is_absolute()
        && path.file_name().and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(STEAM_EXE))
}

fn registry_path(value: Result<Option<(u32, String)>, ()>) -> Option<PathBuf> {
    let (kind, path) = value.ok()??;
    (kind == REG_SZ && !path.is_empty()).then(|| PathBuf::from(path))
}

fn installed_executable() -> Option<PathBuf> {
    let user_exe = registry_path(config::read_reg_string(STEAM_KEY, "SteamExe"));
    let user_dir = registry_path(config::read_reg_string(STEAM_KEY, "SteamPath"));
    let machine_dirs = [KEY_WOW64_32KEY, KEY_WOW64_64KEY].into_iter().filter_map(|view| {
        registry_path(config::read_registry_string(HKEY_LOCAL_MACHINE, STEAM_KEY, "InstallPath", view))
    });
    let default_dirs = ["ProgramFiles(x86)", "ProgramFiles"].into_iter()
        .filter_map(env::var_os).map(|dir| PathBuf::from(dir).join("Steam"));
    user_exe.into_iter()
        .chain(user_dir.into_iter().chain(machine_dirs).chain(default_dirs).map(|dir| dir.join(STEAM_EXE)))
        .find(|path| steam_executable(path) && path.is_file())
}

fn running_executable(process: &OwnedHandle) -> Option<PathBuf> {
    let mut path = vec![0u16; 32_768];
    let mut len = path.len() as u32;
    if unsafe { QueryFullProcessImageNameW(process.as_raw_handle(), 0, path.as_mut_ptr(), &mut len) } == 0 {
        return None;
    }
    let path = PathBuf::from(OsString::from_wide(&path[..len as usize]));
    steam_executable(&path).then_some(path)
}

fn spawn_client(path: &Path, flag: Option<&str>) -> io::Result<Child> {
    let directory = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    Command::new(path).current_dir(directory).args(flag).spawn()
}

fn open_installed() -> Result<Outcome, Error> {
    let executable = installed_executable().ok_or(Error::NotInstalled)?;
    spawn_client(&executable, None).map_err(|_| Error::Launch)?;
    Ok(Outcome::Opened)
}

fn wait_for_exit(handle: RawHandle, timeout_ms: u32) -> Result<(), Error> {
    match unsafe { WaitForSingleObject(handle, timeout_ms) } {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_TIMEOUT => Err(Error::Timeout),
        _ => Err(Error::Check),
    }
}

pub fn perform(request: Request) -> Result<Outcome, Error> {
    let Some(pid) = running_pid().map_err(|_| Error::Check)? else {
        return open_installed();
    };
    if matches!(request, Request::Open) {
        return Ok(Outcome::AlreadyOpen);
    }

    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, 0, pid) };
    if process.is_null() {
        return if !is_running().map_err(|_| Error::Check)? { open_installed() } else { Err(Error::Check) };
    }
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    match unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } {
        WAIT_OBJECT_0 => return open_installed(),
        WAIT_TIMEOUT => {}
        _ => return Err(Error::Check),
    }
    let executable = running_executable(&process).ok_or(Error::Check)?;
    let start = Instant::now();
    let shutdown = spawn_client(&executable, Some("-shutdown")).map_err(|_| Error::Shutdown)?;
    wait_for_exit(process.as_raw_handle(), SHUTDOWN_TIMEOUT_MS)?;
    let remaining_ms = SHUTDOWN_TIMEOUT_MS.saturating_sub(start.elapsed().as_millis().min(u32::MAX as u128) as u32);
    // The shutdown command briefly runs as another steam.exe.
    wait_for_exit(shutdown.as_raw_handle(), remaining_ms)?;
    if !is_running().map_err(|_| Error::Check)? {
        spawn_client(&executable, None).map_err(|_| Error::Launch)?;
    }
    Ok(Outcome::Restarted)
}

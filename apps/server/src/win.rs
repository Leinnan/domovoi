//! Windows calls the server makes that std has no wrapper for: job objects
//! (a child's whole process tree), owner-only file ACLs, process and port
//! lookups, keep-awake, free disk space and registry values.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, LocalFree, STILL_ACTIVE};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GetNamedSecurityInfoW, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation, GetTokenInformation, NO_INHERITANCE,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SUB_CONTAINERS_AND_OBJECTS_INHERIT, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE, TerminateProcess,
};

/// `s` as a NUL-terminated UTF-16 string.
pub fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// A kernel handle, closed when dropped.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the handle was returned open by the call that made it and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

// SAFETY: a kernel handle may be used and closed from any thread.
unsafe impl Send for Owned {}
// SAFETY: as above; the calls made through `&Owned` are thread-safe.
unsafe impl Sync for Owned {}

fn open_process(pid: u32, access: u32) -> io::Result<Owned> {
    // SAFETY: plain call; a null handle is the failure.
    let handle = unsafe { OpenProcess(access, 0, pid) };
    if handle.is_null() { Err(io::Error::last_os_error()) } else { Ok(Owned(handle)) }
}

/// A job object: the processes in it, and every process they start, end
/// when it is closed (and so when this server exits, however it exits).
pub struct Job(Owned);

impl Job {
    /// A job that kills what it holds when closed. `breakaway` lets a
    /// process started from inside it with `CREATE_BREAKAWAY_FROM_JOB` leave.
    pub fn new(breakaway: bool) -> io::Result<Job> {
        // SAFETY: no attributes and no name: a fresh, private job.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Job(Owned(handle));
        // SAFETY: an all-zero JOBOBJECT_EXTENDED_LIMIT_INFORMATION is "no limits".
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | if breakaway { JOB_OBJECT_LIMIT_BREAKAWAY_OK } else { 0 };
        // SAFETY: `info` is the structure this information class takes, with its size.
        let ok = unsafe {
            SetInformationJobObject(
                job.0.0,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    /// Puts the process `pid` (and what it starts from now on) in the job.
    pub fn assign(&self, pid: u32) -> io::Result<()> {
        let process = open_process(pid, PROCESS_SET_QUOTA | PROCESS_TERMINATE)?;
        // SAFETY: both handles are open.
        if unsafe { AssignProcessToJobObject(self.0.0, process.0) } == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }

    /// Ends every process in the job now.
    pub fn terminate(&self) {
        // SAFETY: the handle is open.
        unsafe { TerminateJobObject(self.0.0, 1) };
    }

    /// Whether the process `pid` is in this job.
    #[cfg(test)]
    pub fn holds(&self, pid: u32) -> bool {
        let Ok(process) = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION) else { return false };
        let mut inside = 0;
        // SAFETY: both handles are open and `inside` is a valid out pointer.
        unsafe { windows_sys::Win32::System::JobObjects::IsProcessInJob(process.0, self.0.0, &mut inside) != 0 && inside != 0 }
    }
}

/// Whether the process `pid` is still running.
pub fn process_alive(pid: u32) -> bool {
    let Ok(process) = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION) else {
        return false;
    };
    let mut code = 0u32;
    // SAFETY: the handle is open and `code` is a valid out pointer.
    unsafe { GetExitCodeProcess(process.0, &mut code) != 0 && code == STILL_ACTIVE as u32 }
}

/// Ends the process `pid` (not its children).
pub fn terminate_process(pid: u32) -> io::Result<()> {
    let process = open_process(pid, PROCESS_TERMINATE)?;
    // SAFETY: the handle is open with terminate access.
    if unsafe { TerminateProcess(process.0, 1) } == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

/// The SID of the user this process runs as.
fn current_user_sid() -> io::Result<Vec<u8>> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the pseudo-handle of this process, and a valid out pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Owned(token);
    let mut len = 0u32;
    // SAFETY: asking for the size only.
    unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut len) };
    // u64s, so the TOKEN_USER at its start is aligned.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` holds `len` bytes.
    if unsafe { GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call wrote a TOKEN_USER at the start of `buf`, its SID inside `buf`.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    Ok(sid_bytes(sid))
}

fn sid_bytes(sid: PSID) -> Vec<u8> {
    // SAFETY: `sid` points at a valid SID; its length is in its own header.
    unsafe {
        let len = windows_sys::Win32::Security::GetLengthSid(sid) as usize;
        std::slice::from_raw_parts(sid.cast::<u8>(), len).to_vec()
    }
}

/// A security descriptor returned by `GetNamedSecurityInfoW`, freed when dropped.
struct Descriptor {
    sd: PSECURITY_DESCRIPTOR,
    owner: PSID,
    dacl: *mut ACL,
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: the system allocated it with LocalAlloc for this caller.
        unsafe { LocalFree(self.sd) };
    }
}

fn descriptor(path: &Path, what: u32) -> io::Result<Descriptor> {
    let name = wide(path);
    let mut d = Descriptor { sd: std::ptr::null_mut(), owner: std::ptr::null_mut(), dacl: std::ptr::null_mut() };
    // SAFETY: valid name and out pointers; the descriptor is freed by `Descriptor`.
    let err = unsafe { GetNamedSecurityInfoW(name.as_ptr(), SE_FILE_OBJECT, what, &mut d.owner, std::ptr::null_mut(), &mut d.dacl, std::ptr::null_mut(), &mut d.sd) };
    if err != ERROR_SUCCESS { Err(io::Error::from_raw_os_error(err as i32)) } else { Ok(d) }
}

/// Whether `path` is owned by the user this process runs as.
pub fn owned_by_current_user(path: &Path) -> bool {
    let (Ok(me), Ok(d)) = (current_user_sid(), descriptor(path, OWNER_SECURITY_INFORMATION)) else {
        return false;
    };
    // SAFETY: both point at valid SIDs.
    !d.owner.is_null() && unsafe { EqualSid(d.owner, me.as_ptr() as PSID) } != 0
}

/// Gives `path` an ACL of its own that only this user can use: inherited
/// entries (Administrators, SYSTEM, other users of a shared folder) are
/// dropped. A folder's new contents inherit it. The Windows `chmod 600`/`700`.
pub fn restrict_to_current_user(path: &Path) -> io::Result<()> {
    let mut me = current_user_sid()?;
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS,
        grfAccessMode: SET_ACCESS,
        grfInheritance: if path.is_dir() { SUB_CONTAINERS_AND_OBJECTS_INHERIT } else { NO_INHERITANCE },
        Trustee: TRUSTEE_W { TrusteeForm: TRUSTEE_IS_SID, TrusteeType: TRUSTEE_IS_USER, ptstrName: me.as_mut_ptr().cast(), ..Default::default() },
    };
    let mut acl: *mut ACL = std::ptr::null_mut();
    // SAFETY: one valid entry and no old ACL; the new one is freed below.
    let err = unsafe { SetEntriesInAclW(1, &entry, std::ptr::null(), &mut acl) };
    if err != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(err as i32));
    }
    let name = wide(path);
    // SAFETY: valid name and ACL; only the (protected) DACL changes.
    let err = unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr().cast_mut(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl,
            std::ptr::null(),
        )
    };
    // SAFETY: SetEntriesInAclW allocated it with LocalAlloc.
    unsafe { LocalFree(acl.cast()) };
    if err != ERROR_SUCCESS { Err(io::Error::from_raw_os_error(err as i32)) } else { Ok(()) }
}

/// A security descriptor that gives this user everything and nobody else
/// anything (a protected DACL with one entry), for an object made with it.
pub struct OwnerOnly(PSECURITY_DESCRIPTOR);

impl OwnerOnly {
    pub fn new() -> io::Result<OwnerOnly> {
        use windows_sys::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
        let sid = current_user_sid()?;
        let mut text: windows_sys::core::PWSTR = std::ptr::null_mut();
        // SAFETY: a valid SID and out pointer; the string is freed below.
        if unsafe { ConvertSidToStringSidW(sid.as_ptr() as PSID, &mut text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the call wrote a NUL-terminated string at `text`.
        let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, (0..).take_while(|&i| *text.add(i) != 0).count()) });
        // SAFETY: allocated with LocalAlloc by the call above.
        unsafe { LocalFree(text.cast()) };
        let sddl = wide(format!("D:P(A;;GA;;;{sid})"));
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: a valid SDDL string and out pointer; `OwnerOnly` frees the descriptor.
        if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(OwnerOnly(sd))
    }

    /// Attributes for a `Create…` call; valid while `self` lives.
    pub fn attributes(&self) -> windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
        windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for OwnerOnly {
    fn drop(&mut self) {
        // SAFETY: allocated with LocalAlloc for this caller, freed once.
        unsafe { LocalFree(self.0) };
    }
}

/// Opens `path` for writing, created readable by this user only from the
/// start (no moment with the folder's inherited ACL). `new_only` refuses an
/// existing file; otherwise one is truncated, and given that ACL too.
pub fn create_private(path: &Path, new_only: bool) -> io::Result<std::fs::File> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GENERIC_WRITE, GetLastError, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CREATE_ALWAYS, CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL};
    let owner_only = OwnerOnly::new()?;
    let attributes = owner_only.attributes();
    let name = wide(path);
    let disposition = if new_only { CREATE_NEW } else { CREATE_ALWAYS };
    // SAFETY: a valid name and attributes that outlive the call; the handle goes to a `File`.
    let handle = unsafe { CreateFileW(name.as_ptr(), GENERIC_WRITE, 0, &attributes, disposition, FILE_ATTRIBUTE_NORMAL, std::ptr::null_mut()) };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: plain call, right after the one whose result it qualifies.
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    // SAFETY: a file handle this call opened and nothing else owns.
    let file = unsafe { std::fs::File::from_raw_handle(handle) };
    // An existing file keeps its descriptor: give it this one.
    if existed {
        restrict_to_current_user(path)?;
    }
    Ok(file)
}

/// Whether every entry of `path`'s ACL is for this user (what
/// [`restrict_to_current_user`] leaves). `None` when it can't be read.
pub fn only_current_user_can_access(path: &Path) -> Option<bool> {
    let me = current_user_sid().ok()?;
    let d = descriptor(path, DACL_SECURITY_INFORMATION).ok()?;
    if d.dacl.is_null() {
        // A null DACL lets everyone in.
        return Some(false);
    }
    let mut size = ACL_SIZE_INFORMATION { AceCount: 0, AclBytesInUse: 0, AclBytesFree: 0 };
    // SAFETY: `size` is the structure this class fills, with its size.
    if unsafe { GetAclInformation(d.dacl, (&raw mut size).cast(), std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32, AclSizeInformation) } == 0 {
        return None;
    }
    for i in 0..size.AceCount {
        let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: `i` is below the ACL's entry count.
        if unsafe { GetAce(d.dacl, i, &mut ace) } == 0 {
            return None;
        }
        // Allowed, denied and audit entries all begin like ACCESS_ALLOWED_ACE:
        // a header, a mask, then the SID.
        let ace = ace.cast::<ACCESS_ALLOWED_ACE>();
        // SAFETY: the entry's SID starts at `SidStart` inside the entry.
        let sid = unsafe { (&raw mut (*ace).SidStart).cast::<core::ffi::c_void>() };
        // SAFETY: both point at valid SIDs.
        if unsafe { EqualSid(sid, me.as_ptr() as PSID) } == 0 {
            return Some(false);
        }
    }
    Some(true)
}

/// Keeps the machine from sleeping while it lives (the display may still
/// turn off). The request belongs to a thread, so a thread of its own holds
/// it; Windows drops it when this process exits.
pub struct AwakeGuard {
    release: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AwakeGuard {
    pub fn hold() -> io::Result<AwakeGuard> {
        use windows_sys::Win32::System::Power::{ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState};
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let (started, ready) = std::sync::mpsc::channel::<bool>();
        let thread = std::thread::Builder::new().name("domovoi-keep-awake".into()).spawn(move || {
            // SAFETY: plain calls; 0 is the failure.
            let held = unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) } != 0;
            let _ = started.send(held);
            if held {
                let _ = wait.recv();
                // SAFETY: as above: back to "sleep when idle".
                unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
            }
        })?;
        if ready.recv().unwrap_or(false) {
            Ok(AwakeGuard { release: Some(release), thread: Some(thread) })
        } else {
            let _ = thread.join();
            Err(io::Error::other("SetThreadExecutionState refused"))
        }
    }
}

impl Drop for AwakeGuard {
    fn drop(&mut self) {
        drop(self.release.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Bytes free for this user on the volume holding `path`.
pub fn free_space(path: &Path) -> Option<u64> {
    let name = wide(path);
    let mut free = 0u64;
    // SAFETY: valid name and out pointer; the totals aren't asked for.
    let ok = unsafe { windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(name.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(free)
}

/// The TCP ports listened on at a loopback or wildcard address (what a local
/// dev server listens at), with the process listening: IPv4 and IPv6.
pub fn local_tcp_listeners() -> Vec<(u16, u32)> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER};
    use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};

    fn table(family: u16) -> Option<Vec<u64>> {
        let mut len = 0u32;
        // SAFETY: asking for the size only.
        unsafe { GetExtendedTcpTable(std::ptr::null_mut(), &mut len, 0, u32::from(family), TCP_TABLE_OWNER_PID_LISTENER, 0) };
        // Retried: the table can grow between the two calls.
        for _ in 0..4 {
            let mut buf = vec![0u64; (len as usize).div_ceil(8) + 64];
            let mut size = (buf.len() * 8) as u32;
            // SAFETY: `buf` holds `size` bytes, aligned for the table.
            let err = unsafe { GetExtendedTcpTable(buf.as_mut_ptr().cast(), &mut size, 0, u32::from(family), TCP_TABLE_OWNER_PID_LISTENER, 0) };
            if err == ERROR_SUCCESS {
                return Some(buf);
            }
            len = size;
        }
        None
    }

    fn rows<R>(buf: &[u64]) -> &[R] {
        // The table is a u32 count followed by the rows (each starting 4-byte aligned).
        let count = buf[0] as u32 as usize;
        // SAFETY: the call wrote `count` rows after the count, all inside `buf`.
        unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>().add(4).cast::<R>(), count) }
    }

    let port = |raw: u32| u16::from_be(raw as u16);
    // Addresses are in network order: the first octet is the first byte.
    let local_v4 = |addr: u32| addr == 0 || addr.to_ne_bytes()[0] == 127;
    let local_v6 = |addr: &[u8; 16]| addr.iter().all(|b| *b == 0) || addr[..15].iter().all(|b| *b == 0) && addr[15] == 1;
    let mut out = Vec::new();
    if let Some(buf) = table(AF_INET) {
        out.extend(rows::<MIB_TCPROW_OWNER_PID>(&buf).iter().filter(|r| local_v4(r.dwLocalAddr)).map(|r| (port(r.dwLocalPort), r.dwOwningPid)));
    }
    if let Some(buf) = table(AF_INET6) {
        out.extend(rows::<MIB_TCP6ROW_OWNER_PID>(&buf).iter().filter(|r| local_v6(&r.ucLocalAddr)).map(|r| (port(r.dwLocalPort), r.dwOwningPid)));
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// A string value from the registry (`REG_SZ`, or `REG_EXPAND_SZ` with its
/// `%VARIABLES%` expanded). `None` when it isn't there.
pub fn registry_string(root: windows_sys::Win32::System::Registry::HKEY, key: &str, value: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW};
    let (key, value) = (wide(key), wide(value));
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ;
    let mut len = 0u32;
    // SAFETY: asking for the size only.
    let err = unsafe { RegGetValueW(root, key.as_ptr(), value.as_ptr(), flags, std::ptr::null_mut(), std::ptr::null_mut(), &mut len) };
    if err != ERROR_SUCCESS {
        return None;
    }
    // Expansion can make it longer than the first answer; asked again if so.
    for _ in 0..4 {
        let mut buf = vec![0u16; (len as usize).div_ceil(2) + 1];
        let mut size = (buf.len() * 2) as u32;
        // SAFETY: `buf` holds `size` bytes.
        let err = unsafe { RegGetValueW(root, key.as_ptr(), value.as_ptr(), flags, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut size) };
        if err == ERROR_SUCCESS {
            let chars = (size as usize / 2).min(buf.len());
            let end = buf[..chars].iter().position(|&c| c == 0).unwrap_or(chars);
            return Some(String::from_utf16_lossy(&buf[..end]));
        }
        len = size;
    }
    None
}

/// Whether `path` is a symlink or junction (any reparse point), which the
/// no-follow opens refuse.
pub fn is_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_attributes() & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pids of the processes `parent` started.
    fn children_of(parent: u32) -> Vec<u32> {
        let mut sys = sysinfo::System::new().expect("process list");
        sys.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, true, sysinfo::ProcessRefreshKind::nothing());
        sys.processes().values().filter(|p| p.parent().map(sysinfo::Pid::as_u32) == Some(parent)).map(|p| p.pid().as_u32()).collect()
    }

    /// A secret file is this user's alone from its creation, in a folder
    /// whose ACL lets others in; one written again is truncated and keeps it.
    #[test]
    fn a_private_file_is_private_from_the_start() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("domovoi-private-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("secret");
        create_private(&path, true).unwrap().write_all(b"first").unwrap();
        assert_eq!(only_current_user_can_access(&path), Some(true));
        assert_eq!(only_current_user_can_access(&dir), Some(false), "the folder itself lets others in");
        assert_eq!(create_private(&path, true).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        create_private(&path, false).unwrap().write_all(b"2").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"2");
        assert_eq!(only_current_user_can_access(&path), Some(true));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// What an agent's batch file starts (a `.cmd` shim's `node`) is in the
    /// agent's job too, so it ends with it, wherever this server runs.
    #[test]
    fn a_job_holds_what_its_process_starts() {
        let mut child = crate::process::blocking_command("cmd").args(["/C", "ping -n 30 127.0.0.1 >NUL"]).spawn().expect("cmd starts");
        let job = Job::new(false).expect("a job");
        job.assign(child.id()).expect("assigned");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let grandchild = loop {
            if let Some(pid) = children_of(child.id()).into_iter().next() {
                break pid;
            }
            assert!(std::time::Instant::now() < deadline, "cmd started no ping");
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        assert!(job.holds(grandchild), "ping, started by cmd inside the job, is in the job");
        drop(job);
        child.wait().expect("waited");
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!process_alive(grandchild), "ping ended with the job");
    }

    #[test]
    fn a_job_ends_its_process_when_closed() {
        let started = std::time::Instant::now();
        let mut child = crate::process::blocking_command("cmd").args(["/C", "ping -n 30 127.0.0.1 >NUL"]).spawn().expect("cmd starts");
        let job = Job::new(false).expect("a job");
        job.assign(child.id()).expect("assigned");
        assert!(process_alive(child.id()));
        drop(job);
        child.wait().expect("waited");
        assert!(started.elapsed() < std::time::Duration::from_secs(10), "ended by the job, not by finishing ({:?})", started.elapsed());
    }

    #[test]
    fn a_restricted_file_is_this_users_alone() {
        let dir = std::env::temp_dir().join(format!("domovoi-acl-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("secret");
        std::fs::write(&file, b"x").expect("written");
        assert_eq!(only_current_user_can_access(&file), Some(false), "inherits the temp folder's entries");
        restrict_to_current_user(&file).expect("restricted");
        assert_eq!(only_current_user_can_access(&file), Some(true));
        assert!(owned_by_current_user(&file));
        assert_eq!(std::fs::read(&file).expect("still readable"), b"x");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn this_process_and_its_port_are_seen() {
        assert!(process_alive(std::process::id()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bound");
        let port = listener.local_addr().expect("addr").port();
        assert!(local_tcp_listeners().contains(&(port, std::process::id())));
        assert!(free_space(&std::env::temp_dir()).is_some_and(|b| b > 0));
    }
}

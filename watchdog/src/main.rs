use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::ffi::OsStr;
use std::iter::once;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::JobObjects::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::System::IO::*;

const JOB_OBJECT_MSG_END_OF_JOB_TIME: u32 = 1;
const JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO: u32 = 4;
const JOB_OBJECT_MSG_NEW_PROCESS: u32 = 6;
const JOB_OBJECT_MSG_EXIT_PROCESS: u32 = 7;
const JOB_OBJECT_MSG_ABNORMAL_EXIT_PROCESS: u32 = 8;

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(once(0)).collect()
}

fn main() {
    // Which process to launch under the sandbox. Defaults to notepad for
    // this first milestone; later this becomes the Python interpreter
    // launching UMER's app.py.
    let target: String = std::env::args().nth(1).unwrap_or_else(|| "notepad.exe".into());

    unsafe {
        // 1. Create the job object.
        let job: HANDLE = CreateJobObjectW(null_mut(), null_mut());
        assert!(!job.is_null(), "CreateJobObjectW failed");

        // 2. Kill every process in the job the moment the job handle closes,
        // so nothing survives if this watchdog crashes or exits.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );

        // 3. Create an I/O completion port and associate it with the job.
        // Windows will post a message here on every process create/exit
        // anywhere in the job's tree.
        let port: HANDLE = CreateIoCompletionPort(INVALID_HANDLE_VALUE, null_mut(), 0, 1);
        assert!(!port.is_null(), "CreateIoCompletionPort failed");

        let assoc = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
            CompletionKey: job as *mut c_void,
            CompletionPort: port,
        };
        let ok = SetInformationJobObject(
            job,
            JobObjectAssociateCompletionPortInformation,
            &assoc as *const _ as *const c_void,
            std::mem::size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>() as u32,
        );
        assert_ne!(ok, 0, "associate completion port failed");

        // 4. Launch the target suspended, assign it to the job BEFORE it can
        // run (and spawn anything), then resume it. Doing it in this order
        // closes the race where a fast child process escapes the job.
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();

        let mut cmdline = wide(&target);
        let created = CreateProcessW(
            null_mut(),
            cmdline.as_mut_ptr(),
            null_mut(),
            null_mut(),
            0,
            CREATE_SUSPENDED,
            null_mut(),
            null_mut(),
            &si,
            &mut pi,
        );
        assert_ne!(created, 0, "CreateProcessW failed for {}", target);

        let assigned = AssignProcessToJobObject(job, pi.hProcess);
        assert_ne!(assigned, 0, "AssignProcessToJobObject failed");

        ResumeThread(pi.hThread);

        println!("{{\"event\":\"launched\",\"pid\":{},\"target\":\"{}\"}}", pi.dwProcessId, target);

        // 5. Drain completion port messages until the job goes empty.
        loop {
            let mut bytes: u32 = 0;
            let mut key: usize = 0;
            let mut overlapped: *mut OVERLAPPED = null_mut();

            let ok = GetQueuedCompletionStatus(port, &mut bytes, &mut key, &mut overlapped, u32::MAX);
            if ok == 0 {
                println!("{{\"event\":\"error\",\"detail\":\"GetQueuedCompletionStatus failed\"}}");
                break;
            }

            let pid = bytes; // for these messages, the message field carries the PID
            match key as u32 {
                _ => {}
            }
            match bytes {
                _ => {}
            }
            // The message type comes back as the low DWORD passed via `bytes`
            // is actually the message identifier for job-port notifications;
            // pid is delivered separately as the `overlapped` pointer value.
            let msg = bytes;
            let pid_val = overlapped as usize;

            if msg == JOB_OBJECT_MSG_NEW_PROCESS {
                println!("{{\"event\":\"new_process\",\"pid\":{}}}", pid_val);
            } else if msg == JOB_OBJECT_MSG_EXIT_PROCESS {
                println!("{{\"event\":\"exit_process\",\"pid\":{}}}", pid_val);
            } else if msg == JOB_OBJECT_MSG_ABNORMAL_EXIT_PROCESS {
                println!("{{\"event\":\"abnormal_exit\",\"pid\":{}}}", pid_val);
            } else if msg == JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO {
                println!("{{\"event\":\"job_empty\"}}");
                break;
            }
            let _ = pid;
        }

        CloseHandle(pi.hThread);
        CloseHandle(pi.hProcess);
        CloseHandle(port);
        CloseHandle(job);
    }
}
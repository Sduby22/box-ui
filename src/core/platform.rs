/// Ensure child processes (e.g. the sing-box kernel) are terminated when the GUI exits.
///
/// On Windows this creates a Job Object with `KILL_ON_JOB_CLOSE` and assigns the
/// current process to it.  Every child process automatically joins the same job, so
/// the OS will terminate them all when the GUI exits — whether via normal quit, crash,
/// or force-kill.
///
/// On other platforms this is a no-op (child reaping is handled differently).
pub fn setup_child_process_cleanup() {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::System::JobObjects::*;
        use windows::Win32::System::Threading::GetCurrentProcess;

        unsafe {
            let job = CreateJobObjectW(None, None).expect("CreateJobObjectW failed");

            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags =
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;

            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .expect("SetInformationJobObject failed");

            AssignProcessToJobObject(job, GetCurrentProcess())
                .expect("AssignProcessToJobObject failed");

            // Keep the handle alive until the process exits,
            // at which point the OS triggers KILL_ON_JOB_CLOSE.
            let _ = job;
        }
    }
}

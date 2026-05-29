/// Prepare platform cleanup for child processes such as the sing-box kernel.
///
/// On Windows this creates a dedicated Job Object with `KILL_ON_JOB_CLOSE`.
/// Child processes are assigned to that job after they are spawned.  We avoid
/// assigning the GUI process itself because launchers such as Task Scheduler,
/// IDEs, and service wrappers may already place the app in a Job Object; trying
/// to assign it again can fail and used to terminate the app during startup.
///
/// On other platforms this is a no-op (child reaping is handled differently).
pub fn setup_child_process_cleanup() {
    imp::setup_child_process_cleanup();
}

/// Register a spawned child for platform-level cleanup.
///
/// Failing to register is non-fatal: the app still keeps an explicit `Child`
/// handle and stops the kernel on normal quit.
pub fn register_child_process(child: &std::process::Child) {
    imp::register_child_process(child);
}

#[cfg(target_os = "windows")]
mod imp {
    use std::sync::OnceLock;

    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::*;

    static CHILD_JOB_HANDLE: OnceLock<usize> = OnceLock::new();

    pub fn setup_child_process_cleanup() {
        if CHILD_JOB_HANDLE.get().is_some() {
            return;
        }

        match create_child_job() {
            Ok(handle) => {
                let _ = CHILD_JOB_HANDLE.set(handle);
            }
            Err(e) => {
                tracing::warn!("Windows child-process cleanup is unavailable: {e}");
            }
        }
    }

    pub fn register_child_process(child: &std::process::Child) {
        let Some(&job_raw) = CHILD_JOB_HANDLE.get() else {
            return;
        };

        use std::os::windows::io::AsRawHandle;

        let job = HANDLE(job_raw as *mut core::ffi::c_void);
        let process = HANDLE(child.as_raw_handle());

        unsafe {
            if let Err(e) = AssignProcessToJobObject(job, process) {
                tracing::warn!("Failed to add child process to cleanup job: {e}");
            }
        }
    }

    fn create_child_job() -> windows::core::Result<usize> {
        unsafe {
            let job = CreateJobObjectW(None, None)?;

            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )?;

            // Keep the handle open for the full process lifetime. Windows will
            // close it when the GUI exits, which triggers KILL_ON_JOB_CLOSE.
            Ok(job.0 as usize)
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    pub fn setup_child_process_cleanup() {}

    pub fn register_child_process(_child: &std::process::Child) {}
}

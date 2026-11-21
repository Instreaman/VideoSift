//! Queryable outer Job Object. The library's inner job handles suspend/resume.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::Arc;

use process_wrap::tokio::{CommandWrap, CommandWrapper};
use tokio::process::{Child, Command};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject,
};

#[derive(Debug, Clone)]
pub(super) struct TrackedJob(Arc<OwnedHandle>);

impl TrackedJob {
    pub fn new() -> io::Result<Self> {
        // SAFETY: no name/security pointer; the newly returned handle is owned once.
        let handle = unsafe { CreateJobObjectW(None, None) }.map_err(io::Error::other)?;
        let owned = unsafe { OwnedHandle::from_raw_handle(handle.0) };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the live job and exact typed structure/size remain valid for this call.
        unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as _,
                std::mem::size_of_val(&info) as u32,
            )
        }
        .map_err(io::Error::other)?;
        Ok(Self(Arc::new(owned)))
    }

    pub fn is_empty(&self) -> io::Result<bool> {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: owned handle and correctly sized initialized output buffer.
        unsafe {
            QueryInformationJobObject(
                Some(HANDLE(self.0.as_raw_handle())),
                JobObjectBasicAccountingInformation,
                &mut info as *mut _ as _,
                std::mem::size_of_val(&info) as u32,
                None,
            )
        }
        .map_err(io::Error::other)?;
        Ok(info.ActiveProcesses == 0)
    }
}

impl CommandWrapper for TrackedJob {
    fn post_spawn(
        &mut self,
        _: &mut Command,
        child: &mut Child,
        _: &CommandWrap,
    ) -> io::Result<()> {
        // JobObject's pre_spawn set CREATE_SUSPENDED; all post_spawn hooks run
        // before its wrap_child assigns the inner job and resumes execution.
        let process = child
            .raw_handle()
            .ok_or_else(|| io::Error::other("missing child handle"))?;
        // SAFETY: both handles are live; no child code has run or spawned descendants.
        unsafe { AssignProcessToJobObject(HANDLE(self.0.as_raw_handle()), HANDLE(process)) }
            .map_err(io::Error::other)
    }
}

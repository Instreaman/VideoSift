//! Task lifecycle rules owned by the VideoSift core.

pub mod bilibili;
pub mod local_file;
pub mod task;

pub mod mock;
pub mod mock_run;
pub mod process;
pub mod scheduler;
pub mod storage;

/// Validated sources in the shared VideoSource wire format.
#[derive(Debug, serde::Serialize)]
#[serde(untagged)]
pub enum VideoSource<'a> {
    Local(local_file::LocalSource),
    Bilibili(bilibili::BilibiliSource<'a>),
}

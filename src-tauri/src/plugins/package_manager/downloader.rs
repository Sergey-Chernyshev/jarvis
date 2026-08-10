use std::io::{self, Write};
use std::time::{Duration, Instant};

use jarvis_plugin_protocol::manifest::Digest;
use sha2::{Digest as _, Sha256};
use url::Url;

use super::manager::{ManagerError, ManagerResult};
use super::paths::PluginPaths;
use super::quarantine::{open_quarantine_parent, QuarantineArchiveRef, MAX_ARCHIVE_BYTES};
use super::random_storage_id;

const DEFAULT_DOWNLOAD_DEADLINE: Duration = Duration::from_secs(120);

pub trait Downloader: Send + Sync {
    fn download(&self, url: &str, output: &mut dyn Write, deadline: Duration) -> ManagerResult<()>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DownloadLimits {
    pub max_bytes: u64,
    pub deadline: Duration,
}

impl Default for DownloadLimits {
    fn default() -> Self {
        Self {
            max_bytes: MAX_ARCHIVE_BYTES,
            deadline: DEFAULT_DOWNLOAD_DEADLINE,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedArchive {
    pub archive: QuarantineArchiveRef,
    pub archive_digest: Digest,
}

#[derive(Clone, Debug)]
pub struct HttpDownloader {
    client: reqwest::blocking::Client,
}

impl HttpDownloader {
    pub fn new(timeout: Duration) -> ManagerResult<Self> {
        let timeout = timeout.min(DEFAULT_DOWNLOAD_DEADLINE);
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(timeout.min(Duration::from_secs(15)))
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                let target = attempt.url();
                if attempt.previous().len() >= 3
                    || target.scheme() != "https"
                    || !target.username().is_empty()
                    || target.password().is_some()
                    || target.host_str().is_none()
                {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(|error| ManagerError::new("download_client", error.to_string()))?;
        Ok(Self { client })
    }
}

impl Downloader for HttpDownloader {
    fn download(&self, url: &str, output: &mut dyn Write, deadline: Duration) -> ManagerResult<()> {
        let parsed = Url::parse(url)
            .map_err(|_| ManagerError::new("download_url", "invalid package URL"))?;
        if parsed.scheme() != "https"
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.host_str().is_none()
        {
            return Err(ManagerError::new(
                "download_url",
                "catalog packages require an HTTPS URL without embedded credentials",
            ));
        }
        let mut response = self
            .client
            .get(parsed)
            .timeout(deadline.min(DEFAULT_DOWNLOAD_DEADLINE))
            .send()
            .map_err(|error| ManagerError::new("download_failed", error.to_string()))?;
        if !response.status().is_success() {
            return Err(ManagerError::new(
                "download_status",
                format!("package server returned {}", response.status()),
            ));
        }
        io::copy(&mut response, &mut *output)
            .map_err(|error| ManagerError::new("download_io", error.to_string()))?;
        Ok(())
    }
}

pub fn stage_download(
    paths: &PluginPaths,
    url: &str,
    downloader: &dyn Downloader,
    limits: DownloadLimits,
) -> ManagerResult<StagedArchive> {
    if limits.max_bytes == 0
        || limits.max_bytes > MAX_ARCHIVE_BYTES
        || limits.deadline.is_zero()
        || limits.deadline > DEFAULT_DOWNLOAD_DEADLINE
    {
        return Err(ManagerError::new(
            "download_limits",
            "download limits exceed the package quarantine policy",
        ));
    }
    paths.prepare().map_err(ManagerError::from)?;
    let parent = open_quarantine_parent(paths)?;
    let archive_name = format!("package-{}.jarvis-plugin", random_storage_id()?);
    let mut file = parent.create_archive(&archive_name)?;
    let mut writer = BoundedDigestWriter {
        output: &mut file,
        digest: Sha256::new(),
        written: 0,
        maximum: limits.max_bytes,
        started: Instant::now(),
        deadline: limits.deadline,
    };
    let download_result = downloader.download(url, &mut writer, limits.deadline);
    let written = writer.written;
    let elapsed = writer.started.elapsed();
    let digest = writer.finish();

    let result = (|| {
        download_result?;
        if elapsed > limits.deadline {
            return Err(ManagerError::new(
                "download_deadline",
                "package download exceeded its deadline",
            ));
        }
        if written == 0 {
            return Err(ManagerError::new(
                "download_empty",
                "package download produced an empty archive",
            ));
        }
        file.flush()
            .map_err(|error| ManagerError::new("download_io", error.to_string()))?;
        file.sync_all()
            .map_err(|error| ManagerError::new("download_sync", error.to_string()))?;
        let archive = parent.record_archive(archive_name.clone(), &file)?;
        parent.sync()?;
        let digest = Digest::new(format!("sha256:{}", encode_hex(&digest)))
            .map_err(|_| ManagerError::new("download_digest", "invalid SHA-256 digest"))?;
        Ok(StagedArchive {
            archive,
            archive_digest: digest,
        })
    })();

    if result.is_err() {
        drop(file);
        let _ = parent.unlink_archive(&archive_name);
    }
    result
}

struct BoundedDigestWriter<'a> {
    output: &'a mut std::fs::File,
    digest: Sha256,
    written: u64,
    maximum: u64,
    started: Instant,
    deadline: Duration,
}

impl BoundedDigestWriter<'_> {
    fn finish(self) -> [u8; 32] {
        self.digest.finalize().into()
    }
}

impl Write for BoundedDigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.started.elapsed() > self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "package download deadline exceeded",
            ));
        }
        let requested = u64::try_from(bytes.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "download chunk too large"))?;
        let next = self
            .written
            .checked_add(requested)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "download size overflow"))?;
        if next > self.maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "package archive exceeds its size limit",
            ));
        }
        self.output.write_all(bytes)?;
        self.digest.update(bytes);
        self.written = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing into String cannot fail");
    }
    output
}

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, ensure};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;

const RUNTIME_MAX_LINES_PER_FILE: usize = 5_000;
const RUNTIME_MAX_FILES: usize = 2;

pub fn init_json_logs(path: &Path) -> Result<WorkerGuard> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create log directory: {}", parent.display()))?;
    }

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open log file: {}", path.display()))?;

    init_json_logs_with_writer(file)
}

pub fn init_runtime_json_logs(base_path: &Path) -> Result<WorkerGuard> {
    let writer =
        TimestampedJsonLogWriter::new(base_path, RUNTIME_MAX_LINES_PER_FILE, RUNTIME_MAX_FILES)?;
    init_json_logs_with_writer(writer)
}

fn init_json_logs_with_writer<W>(writer: W) -> Result<WorkerGuard>
where
    W: Write + Send + 'static,
{
    let (non_blocking, guard) = tracing_appender::non_blocking(writer);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    fmt()
        .with_env_filter(filter)
        .json()
        .with_current_span(false)
        .with_span_list(false)
        .with_writer(non_blocking)
        .init();

    Ok(guard)
}

struct TimestampedJsonLogWriter {
    state: TimestampedJsonLogState,
}

struct TimestampedJsonLogState {
    directory: PathBuf,
    file_stem: String,
    file_extension: String,
    max_lines_per_file: usize,
    max_files: usize,
    current_file: File,
    current_line_count: usize,
    pending_bytes: Vec<u8>,
    last_timestamp_micros: u128,
}

impl TimestampedJsonLogWriter {
    fn new(base_path: &Path, max_lines_per_file: usize, max_files: usize) -> Result<Self> {
        ensure!(
            max_lines_per_file > 0,
            "max_lines_per_file must be greater than zero"
        );
        ensure!(max_files > 0, "max_files must be greater than zero");

        let directory = base_path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        fs::create_dir_all(&directory).with_context(|| {
            format!(
                "failed to create runtime log directory: {}",
                directory.display()
            )
        })?;

        let file_stem = base_path
            .file_stem()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("events")
            .to_owned();
        let file_extension = base_path
            .extension()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("jsonl")
            .to_owned();

        let mut state = TimestampedJsonLogState {
            directory,
            file_stem,
            file_extension,
            max_lines_per_file,
            max_files,
            current_file: open_placeholder_file()?,
            current_line_count: 0,
            pending_bytes: Vec::new(),
            last_timestamp_micros: 0,
        };
        state.rotate_file()?;

        Ok(Self { state })
    }
}

impl TimestampedJsonLogState {
    fn write_buffer(&mut self, buf: &[u8]) -> io::Result<()> {
        self.pending_bytes.extend_from_slice(buf);

        while let Some(position) = self.pending_bytes.iter().position(|byte| *byte == b'\n') {
            if self.current_line_count >= self.max_lines_per_file {
                self.rotate_file()?;
            }

            let line = self.pending_bytes.drain(..=position).collect::<Vec<u8>>();
            self.current_file.write_all(&line)?;
            self.current_line_count += 1;
        }

        Ok(())
    }

    fn rotate_file(&mut self) -> io::Result<()> {
        self.current_file.flush()?;

        let timestamp = self.next_timestamp_micros()?;
        let path = self.current_log_path(timestamp);
        self.current_file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        self.current_line_count = 0;
        self.prune_old_logs()?;
        Ok(())
    }

    fn next_timestamp_micros(&mut self) -> io::Result<u128> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?
            .as_micros();
        let next = now.max(self.last_timestamp_micros.saturating_add(1));
        self.last_timestamp_micros = next;
        Ok(next)
    }

    fn current_log_path(&self, timestamp_micros: u128) -> PathBuf {
        self.directory.join(format!(
            "{}-{}.{}",
            self.file_stem, timestamp_micros, self.file_extension
        ))
    }

    fn prune_old_logs(&self) -> io::Result<()> {
        let mut matching_files = fs::read_dir(&self.directory)?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let file_type = entry.file_type().ok()?;
                if !file_type.is_file() {
                    return None;
                }

                let file_name = entry.file_name();
                if !self.is_runtime_log_file(&file_name) {
                    return None;
                }
                Some((file_name, entry.path()))
            })
            .collect::<Vec<_>>();

        matching_files.sort_by(|left, right| left.0.cmp(&right.0));
        let excess = matching_files.len().saturating_sub(self.max_files);
        for (_, path) in matching_files.into_iter().take(excess) {
            fs::remove_file(path)?;
        }

        Ok(())
    }

    fn is_runtime_log_file(&self, file_name: &OsString) -> bool {
        let file_name = file_name.to_string_lossy();
        let prefix = format!("{}-", self.file_stem);
        let suffix = format!(".{}", self.file_extension);

        file_name.starts_with(&prefix)
            && file_name.ends_with(&suffix)
            && file_name.len() > prefix.len() + suffix.len()
            && file_name[prefix.len()..file_name.len() - suffix.len()]
                .bytes()
                .all(|byte| byte.is_ascii_digit())
    }
}

impl Write for TimestampedJsonLogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.state.write_buffer(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.state.current_file.flush()
    }
}

fn open_placeholder_file() -> Result<File> {
    #[cfg(unix)]
    let path = "/dev/null";
    #[cfg(windows)]
    let path = "NUL";

    OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|error| anyhow!(error))
        .with_context(|| format!("failed to open placeholder log sink: {path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("threadbridge-logging-{name}-{unique}"));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn runtime_log_paths(dir: &Path) -> Vec<PathBuf> {
        let mut paths = fs::read_dir(dir)
            .expect("read temp dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|value| value.to_str())
                    .map(|value| value.starts_with("events-") && value.ends_with(".jsonl"))
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    #[test]
    fn runtime_writer_creates_timestamped_log_file() {
        let dir = temp_dir("create");
        let mut writer =
            TimestampedJsonLogWriter::new(&dir.join("events.jsonl"), 2, 2).expect("new writer");

        writer
            .write_all(br#"{"event":"startup"}"#)
            .expect("write partial line");
        writer.write_all(b"\n").expect("write newline");
        writer.flush().expect("flush writer");

        let paths = runtime_log_paths(&dir);
        assert_eq!(paths.len(), 1);
        let content = fs::read_to_string(&paths[0]).expect("read runtime log");
        assert_eq!(content, "{\"event\":\"startup\"}\n");
    }

    #[test]
    fn runtime_writer_rotates_after_reaching_line_limit() {
        let dir = temp_dir("rotate");
        let mut writer =
            TimestampedJsonLogWriter::new(&dir.join("events.jsonl"), 2, 2).expect("new writer");

        writer
            .write_all(b"first\nsecond\nthird\n")
            .expect("write lines");
        writer.flush().expect("flush writer");

        let paths = runtime_log_paths(&dir);
        assert_eq!(paths.len(), 2);

        let first = fs::read_to_string(&paths[0]).expect("read first log");
        let second = fs::read_to_string(&paths[1]).expect("read second log");
        assert_eq!(first, "first\nsecond\n");
        assert_eq!(second, "third\n");
    }

    #[test]
    fn runtime_writer_prunes_to_latest_two_files() {
        let dir = temp_dir("prune");
        let mut writer =
            TimestampedJsonLogWriter::new(&dir.join("events.jsonl"), 2, 2).expect("new writer");

        writer
            .write_all(b"line-1\nline-2\nline-3\nline-4\nline-5\n")
            .expect("write lines");
        writer.flush().expect("flush writer");

        let paths = runtime_log_paths(&dir);
        assert_eq!(paths.len(), 2);

        let contents = paths
            .iter()
            .map(|path| fs::read_to_string(path).expect("read log content"))
            .collect::<Vec<_>>();
        assert_eq!(
            contents,
            vec!["line-3\nline-4\n".to_owned(), "line-5\n".to_owned()]
        );
    }
}

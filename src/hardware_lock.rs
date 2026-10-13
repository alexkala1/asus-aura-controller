//! Advisory device locks live as long as the controller's open file.
use std::fs::File;
use std::io;
use std::time::{Duration, Instant};

pub fn acquire(file: &File, path: &str) -> io::Result<()> {
    acquire_with_timeout(file, path, Duration::from_secs(5))
}

fn acquire_with_timeout(file: &File, path: &str, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("timed out waiting for exclusive access to {path} (another luminactl process is using it)"),
                    ));
                }
                std::thread::sleep(remaining.min(Duration::from_millis(25)));
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("exclusive access to {path}: {error}"),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;

    #[test]
    fn conflicts_times_out_and_releases_on_drop() {
        let path = std::env::temp_dir().join(format!("aura-device-lock-{}", std::process::id()));
        let first = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let second = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let path_str = path.to_str().unwrap();
        acquire(&first, path_str).unwrap();
        let timeout = Duration::from_millis(50);
        let started = Instant::now();
        let error = acquire_with_timeout(&second, path_str, timeout).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() >= timeout);
        assert!(error.to_string().contains(path_str));
        assert!(error
            .to_string()
            .contains("another luminactl process is using it"));
        drop(first);
        acquire_with_timeout(&second, path_str, timeout).unwrap();
        drop(second);
        std::fs::remove_file(path).unwrap();
    }
}

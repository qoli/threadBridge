use std::io;

pub fn process_exists(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }

    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        return true;
    }

    match io::Error::last_os_error().raw_os_error() {
        Some(code) if code == libc::EPERM => true,
        Some(code) if code == libc::ESRCH => false,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::process_exists;

    #[test]
    fn process_exists_returns_true_for_current_process() {
        assert!(process_exists(std::process::id()));
    }

    #[test]
    fn process_exists_rejects_zero_pid() {
        assert!(!process_exists(0));
    }

    #[test]
    fn process_exists_returns_false_for_missing_pid() {
        assert!(!process_exists(i32::MAX as u32));
    }
}

//! Owned subprocess groups for adapters that execute provider CLIs.
use tokio::process::{Child, Command};

/// Keep this guard alive until all child IO has completed. Dropping it terminates
/// descendants even if the direct child has already exited. On non-Unix hosts,
/// Tokio's kill-on-drop still terminates the direct child.
pub struct ProcessGroup(Option<u32>);
impl ProcessGroup {
    pub fn spawn(command: &mut Command) -> std::io::Result<(Child, Self)> {
        command.kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let child = command.spawn()?;
        let group = Self(child.id());
        Ok((child, group))
    }
}
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if let Some(_id) = self.0 {
            #[cfg(unix)]
            unsafe {
                libc::killpg(_id as i32, libc::SIGKILL);
            }
        }
    }
}

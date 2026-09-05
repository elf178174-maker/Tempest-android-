//! Shared `std::process`-based backend used by both the Linux and Android
//! platforms. The two differ only in their execute policy (Android may only
//! `execve()` files under `nativeLibraryDir`), which is injected as a closure.

use super::process::{ProcessBackend, ProcessHandle, ProcessSpec, ProcessStatus};
use crate::{Result, TempestError};
use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

const TERMINATE_GRACE: std::time::Duration = std::time::Duration::from_secs(5);
/// Cap the in-memory output buffer so a chatty Wine process cannot grow the
/// heap without bound while the user is not looking at the log screen.
const MAX_BUFFERED_LINES: usize = 4096;

pub struct UnixProcessHandle {
    child: std::process::Child,
    output: Receiver<String>,
    buffered: Vec<String>,
    finished: Option<ProcessStatus>,
    label: String,
    live: Arc<Mutex<HashSet<String>>>,
}

impl UnixProcessHandle {
    fn record(&mut self, status: ProcessStatus) -> ProcessStatus {
        if !status.is_running() {
            self.finished = Some(status.clone());
            if let Ok(mut live) = self.live.lock() {
                live.remove(&self.label);
            }
        }
        status
    }

    fn pump(&mut self) {
        while let Ok(line) = self.output.try_recv() {
            if self.buffered.len() >= MAX_BUFFERED_LINES {
                self.buffered.remove(0);
            }
            self.buffered.push(line);
        }
    }
}

fn classify(status: std::process::ExitStatus) -> ProcessStatus {
    use std::os::unix::process::ExitStatusExt;
    match status.code() {
        Some(c) => ProcessStatus::Exited(c),
        None => ProcessStatus::Signalled(status.signal().unwrap_or(0)),
    }
}

impl ProcessHandle for UnixProcessHandle {
    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn poll(&mut self) -> Result<ProcessStatus> {
        self.pump();
        if let Some(done) = &self.finished {
            return Ok(done.clone());
        }
        match self.child.try_wait() {
            Ok(Some(status)) => Ok(self.record(classify(status))),
            Ok(None) => Ok(ProcessStatus::Running),
            Err(e) => Err(TempestError::Process(format!(
                "could not poll {}: {e}",
                self.label
            ))),
        }
    }

    fn wait(&mut self) -> Result<ProcessStatus> {
        if let Some(done) = &self.finished {
            return Ok(done.clone());
        }
        let status = self
            .child
            .wait()
            .map_err(|e| TempestError::Process(format!("wait on {} failed: {e}", self.label)))?;
        self.pump();
        Ok(self.record(classify(status)))
    }

    fn terminate(&mut self) -> Result<()> {
        if self.finished.is_some() {
            return Ok(());
        }
        let pid = self.child.id() as i32;
        // SAFETY: `pid` came from a child we spawned and have not yet reaped, so
        // the pid cannot have been recycled onto an unrelated process.
        unsafe { libc::kill(pid, libc::SIGTERM) };

        let deadline = std::time::Instant::now() + TERMINATE_GRACE;
        while std::time::Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                self.record(classify(status));
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        tracing::warn!("{} ignored SIGTERM, sending SIGKILL", self.label);
        self.child.kill().ok();
        let status = self.child.wait().map(classify);
        if let Ok(s) = status {
            self.record(s);
        }
        Ok(())
    }

    fn drain_output(&mut self) -> Vec<String> {
        self.pump();
        std::mem::take(&mut self.buffered)
    }
}

impl Drop for UnixProcessHandle {
    fn drop(&mut self) {
        if self.finished.is_none() {
            self.terminate().ok();
        }
        if let Ok(mut live) = self.live.lock() {
            live.remove(&self.label);
        }
    }
}

/// Decides whether a path may be handed to `execve()` on this platform.
pub type ExecPolicy = Arc<dyn Fn(&Path) -> bool + Send + Sync>;

pub struct UnixProcessBackend {
    live: Arc<Mutex<HashSet<String>>>,
    exec_policy: ExecPolicy,
    policy_explanation: String,
}

impl UnixProcessBackend {
    pub fn new(exec_policy: ExecPolicy, policy_explanation: impl Into<String>) -> Self {
        Self {
            live: Arc::new(Mutex::new(HashSet::new())),
            exec_policy,
            policy_explanation: policy_explanation.into(),
        }
    }

    /// Policy that allows anything — the desktop case.
    pub fn permissive() -> Self {
        Self::new(Arc::new(|_: &Path| true), "no execution restrictions")
    }

    /// Policy that only allows files under `native_bin`, which is what Android
    /// enforces for apps targeting API 29 and above.
    pub fn restricted_to(native_bin: PathBuf) -> Self {
        let dir = native_bin.clone();
        Self::new(
            Arc::new(move |p: &Path| p.starts_with(&dir)),
            format!(
                "Android only permits execve() of files under the app's native \
                 library directory ({}); binaries written to app data can be \
                 mapped but not executed",
                native_bin.display()
            ),
        )
    }
}

impl ProcessBackend for UnixProcessBackend {
    fn spawn(&self, mut spec: ProcessSpec) -> Result<Box<dyn ProcessHandle>> {
        // A bare name like "wine" is resolved against the spec's own PATH, not
        // the host's. Inside the guest container this never happens (the guest
        // resolves its own commands), but on the desktop the caller may
        // legitimately say "wine" and mean "whatever is on PATH".
        if spec.program.components().count() == 1 {
            match resolve_on_path(&spec.program, spec.env.get("PATH").map(String::as_str)) {
                Some(found) => spec.program = found,
                None => {
                    return Err(TempestError::missing(
                        spec.program.display().to_string(),
                        "not found on PATH",
                    ))
                }
            }
        }
        if !spec.program.exists() {
            return Err(TempestError::missing(
                spec.program.display().to_string(),
                "file does not exist",
            ));
        }
        if !self.can_execute(&spec.program) {
            return Err(TempestError::Process(format!(
                "refusing to execute {}: {}",
                spec.program.display(),
                self.policy_explanation
            )));
        }

        tracing::info!("spawn: {}", spec.redacted_display());

        let mut cmd = std::process::Command::new(&spec.program);
        cmd.args(&spec.args);
        // Start from a clean environment so nothing from the Android app
        // process (or the desktop shell) leaks into the guest by accident.
        cmd.env_clear();
        cmd.envs(&spec.env);
        if let Some(dir) = &spec.working_dir {
            cmd.current_dir(dir);
        }

        let (tx, rx) = std::sync::mpsc::channel();
        if spec.capture_output {
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());
        } else {
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::null());
        }
        cmd.stdin(std::process::Stdio::null());

        let mut child = cmd.spawn().map_err(|e| {
            TempestError::Process(format!(
                "failed to start {} ({}): {e}",
                spec.label,
                spec.program.display()
            ))
        })?;

        for (stream, tag) in [
            (child.stdout.take().map(StreamKind::Out), "out"),
            (child.stderr.take().map(StreamKind::Err), "err"),
        ] {
            let Some(stream) = stream else { continue };
            let tx = tx.clone();
            let label = spec.label.clone();
            let tag = tag.to_string();
            std::thread::spawn(move || {
                let reader: Box<dyn std::io::Read + Send> = match stream {
                    StreamKind::Out(s) => Box::new(s),
                    StreamKind::Err(s) => Box::new(s),
                };
                for line in BufReader::new(reader)
                    .lines()
                    .map_while(std::result::Result::ok)
                {
                    let line = crate::logging::redact(&line);
                    crate::logging::guest_line(&label, &tag, &line);
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);

        if let Ok(mut live) = self.live.lock() {
            live.insert(spec.label.clone());
        }

        Ok(Box::new(UnixProcessHandle {
            child,
            output: rx,
            buffered: Vec::new(),
            finished: None,
            label: spec.label,
            live: Arc::clone(&self.live),
        }))
    }

    fn is_running(&self, label: &str) -> bool {
        self.live.lock().map(|l| l.contains(label)).unwrap_or(false)
    }

    fn terminate_all(&self) {
        // Handles own their children; dropping them terminates. This clears the
        // registry so a stale label cannot report a dead process as running.
        if let Ok(mut live) = self.live.lock() {
            live.clear();
        }
    }

    fn can_execute(&self, path: &Path) -> bool {
        (self.exec_policy)(path)
    }
}

/// Find an executable by name on a PATH, without shelling out to `which`.
fn resolve_on_path(program: &Path, path_var: Option<&str>) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = path_var
        .map(String::from)
        .or_else(|| std::env::var("PATH").ok())?;
    path.split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join(program))
        .find(|candidate| {
            candidate
                .metadata()
                .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
}

enum StreamKind {
    Out(std::process::ChildStdout),
    Err(std::process::ChildStderr),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> ProcessSpec {
        ProcessSpec::new("test", "/bin/sh")
            .arg("-c")
            .arg(script)
            .env("PATH", "/usr/bin:/bin")
    }

    #[test]
    fn spawn_captures_output_and_exit_code() {
        let backend = UnixProcessBackend::permissive();
        let mut h = backend
            .spawn(sh("echo hello; echo oops >&2; exit 3"))
            .unwrap();
        let status = h.wait().unwrap();
        assert_eq!(status, ProcessStatus::Exited(3));
        std::thread::sleep(std::time::Duration::from_millis(150));
        let out = h.drain_output();
        assert!(out.iter().any(|l| l == "hello"), "stdout missing: {out:?}");
        assert!(out.iter().any(|l| l == "oops"), "stderr missing: {out:?}");
    }

    #[test]
    fn is_running_tracks_children_without_pgrep() {
        let backend = UnixProcessBackend::permissive();
        let mut h = backend
            .spawn(
                ProcessSpec::new("sleeper", "/bin/sh")
                    .arg("-c")
                    .arg("sleep 30"),
            )
            .unwrap();
        assert!(backend.is_running("sleeper"));
        h.terminate().unwrap();
        assert!(!backend.is_running("sleeper"));
    }

    #[test]
    fn terminate_reports_the_signal() {
        let backend = UnixProcessBackend::permissive();
        let mut h = backend
            .spawn(
                ProcessSpec::new("sleeper", "/bin/sh")
                    .arg("-c")
                    .arg("sleep 30"),
            )
            .unwrap();
        h.terminate().unwrap();
        let status = h.poll().unwrap();
        assert!(!status.is_running(), "still running after terminate");
    }

    #[test]
    fn a_bare_command_name_is_resolved_against_the_spec_path() {
        let backend = UnixProcessBackend::permissive();
        let mut h = backend
            .spawn(
                ProcessSpec::new("echoer", "sh")
                    .arg("-c")
                    .arg("echo resolved")
                    .env("PATH", "/usr/bin:/bin"),
            )
            .unwrap();
        assert_eq!(h.wait().unwrap(), ProcessStatus::Exited(0));
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(h.drain_output().iter().any(|l| l == "resolved"));
    }

    #[test]
    fn a_bare_name_that_is_not_on_path_reports_that_specifically() {
        let backend = UnixProcessBackend::permissive();
        match backend.spawn(
            ProcessSpec::new("nope", "definitely-not-a-real-command").env("PATH", "/usr/bin:/bin"),
        ) {
            Ok(_) => panic!("resolved a command that does not exist"),
            Err(e) => {
                assert_eq!(e.kind(), "missing");
                assert!(e.to_string().contains("PATH"), "{e}");
            }
        }
    }

    #[test]
    fn restricted_policy_refuses_paths_outside_the_native_dir() {
        let backend = UnixProcessBackend::restricted_to(PathBuf::from("/nativelibs"));
        assert!(!backend.can_execute(Path::new("/data/data/app/files/box64")));
        assert!(backend.can_execute(Path::new("/nativelibs/libproot.so")));

        let msg = match backend.spawn(sh("echo hi")) {
            Ok(_) => panic!("restricted backend executed a forbidden path"),
            Err(e) => e.to_string(),
        };
        assert!(
            msg.contains("native library directory"),
            "error should explain the Android restriction, got: {msg}"
        );
    }

    #[test]
    fn missing_program_is_reported_as_missing_not_as_a_generic_io_error() {
        let backend = UnixProcessBackend::permissive();
        match backend.spawn(ProcessSpec::new("nope", "/definitely/not/here")) {
            Ok(_) => panic!("spawned a program that does not exist"),
            Err(e) => assert_eq!(e.kind(), "missing"),
        }
    }

    #[test]
    fn environment_is_not_inherited_from_the_host() {
        std::env::set_var("TEMPEST_LEAK_CANARY", "leaked");
        let backend = UnixProcessBackend::permissive();
        let mut h = backend
            .spawn(sh("echo \"[${TEMPEST_LEAK_CANARY}]\""))
            .unwrap();
        h.wait().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(150));
        let out = h.drain_output().join("\n");
        assert!(out.contains("[]"), "host env leaked into child: {out}");
    }
}

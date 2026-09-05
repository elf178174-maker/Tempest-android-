use crate::Result;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// A process the core wants started.
///
/// Deliberately *not* a shell string: every argument is passed as a distinct
/// element so no part of it can be reinterpreted by `sh -c`. Upstream built
/// shell strings with `format!` and ran them through `sh -c`; combined with
/// values that come from a `vortex://` URI that was a command-injection risk,
/// so the shell is gone entirely.
#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub working_dir: Option<PathBuf>,
    /// Human-readable tag used for logging and for [`ProcessBackend::is_running`].
    pub label: String,
    /// Capture stdout/stderr rather than letting them go to the void.
    pub capture_output: bool,
}

impl ProcessSpec {
    pub fn new(label: impl Into<String>, program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            working_dir: None,
            label: label.into(),
            capture_output: true,
        }
    }

    pub fn arg(mut self, a: impl Into<String>) -> Self {
        self.args.push(a.into());
        self
    }

    pub fn args<I, S>(mut self, it: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(it.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.env.insert(k.into(), v.into());
        self
    }

    pub fn envs<I, K, V>(mut self, it: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.env
            .extend(it.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }

    /// Discard stdout/stderr instead of buffering them. Used for long-lived
    /// helpers whose output would drown the session log.
    pub fn no_capture(mut self) -> Self {
        self.capture_output = false;
        self
    }

    pub fn working_dir(mut self, d: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(d.into());
        self
    }

    /// Render the command the way it would appear in a log, with values of
    /// sensitive-looking environment variables and URI tokens masked.
    pub fn redacted_display(&self) -> String {
        let mut s = String::new();
        for (k, v) in &self.env {
            let shown = if crate::logging::is_sensitive_key(k) {
                "***".to_string()
            } else {
                v.clone()
            };
            s.push_str(&format!("{k}={shown} "));
        }
        s.push_str(&self.program.display().to_string());
        for a in &self.args {
            s.push(' ');
            s.push_str(&crate::logging::redact(a));
        }
        s
    }
}

/// Why a process is no longer running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessStatus {
    Running,
    Exited(i32),
    /// Killed by a signal (Unix). Carries the signal number when known.
    Signalled(i32),
}

impl ProcessStatus {
    pub fn is_running(&self) -> bool {
        matches!(self, ProcessStatus::Running)
    }

    /// A human-readable explanation, including the common Wine/translation
    /// failure codes so the UI never has to show a bare number.
    pub fn explain(&self) -> String {
        match self {
            ProcessStatus::Running => "running".to_string(),
            ProcessStatus::Exited(0) => "exited cleanly".to_string(),
            ProcessStatus::Exited(1) => {
                "exited with code 1 — the guest program reported a general failure; \
                 check the log for the last Wine error line"
                    .to_string()
            }
            ProcessStatus::Exited(126) => {
                "exit code 126 — a binary could not be executed. On Android this \
                 almost always means a file outside the app's native library \
                 directory was exec()'d, which the platform forbids."
                    .to_string()
            }
            ProcessStatus::Exited(127) => {
                "exit code 127 — a program or shared library was not found inside \
                 the guest filesystem; the runtime install is probably incomplete"
                    .to_string()
            }
            ProcessStatus::Exited(c) => format!("exited with code {c}"),
            ProcessStatus::Signalled(11) => {
                "killed by SIGSEGV — the translation layer or the graphics driver \
                 crashed; capture the log and check the Vulkan driver"
                    .to_string()
            }
            ProcessStatus::Signalled(9) => {
                "killed by SIGKILL — most likely Android's low-memory killer \
                 reclaimed the process while the app was in the background"
                    .to_string()
            }
            ProcessStatus::Signalled(s) => format!("killed by signal {s}"),
        }
    }
}

/// A running child.
pub trait ProcessHandle: Send {
    fn pid(&self) -> u32;
    /// Non-blocking status poll.
    fn poll(&mut self) -> Result<ProcessStatus>;
    /// Block until exit.
    fn wait(&mut self) -> Result<ProcessStatus>;
    /// Ask politely (SIGTERM), then give up after a grace period and SIGKILL.
    fn terminate(&mut self) -> Result<()>;
    /// Drain any output captured since the last call. Empty when
    /// `capture_output` was false.
    fn drain_output(&mut self) -> Vec<String>;
}

/// How processes are started and observed on this platform.
pub trait ProcessBackend: Send + Sync {
    fn spawn(&self, spec: ProcessSpec) -> Result<Box<dyn ProcessHandle>>;

    /// Whether a process with the given label is currently alive.
    ///
    /// Upstream shelled out to `pgrep -f receiver.exe`. Android has no usable
    /// `pgrep` (and `/proc` is restricted to your own processes since API 29),
    /// so implementations track the children they started instead of scanning
    /// the process table.
    fn is_running(&self, label: &str) -> bool;

    /// Best-effort stop of every child this backend started.
    fn terminate_all(&self);

    /// Whether this backend can execute a file at the given path at all.
    /// Android returns false for anything outside `nativeLibraryDir`, which
    /// lets callers produce a precise error instead of a confusing EACCES.
    fn can_execute(&self, path: &std::path::Path) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacted_display_masks_tokens_and_secret_env() {
        let spec = ProcessSpec::new("wine", "/usr/bin/wine")
            .arg("Vortex.exe")
            .arg("vortex://play?game=4&token=supersecret")
            .env("WINEPREFIX", "/data/prefix")
            .env("VORTEX_SESSION_TOKEN", "supersecret");
        let shown = spec.redacted_display();
        assert!(!shown.contains("supersecret"), "leaked a token: {shown}");
        assert!(shown.contains("WINEPREFIX=/data/prefix"));
        assert!(shown.contains("game=4"));
    }

    #[test]
    fn status_explanations_are_actionable() {
        assert!(ProcessStatus::Exited(126)
            .explain()
            .contains("native library"));
        assert!(ProcessStatus::Signalled(9).explain().contains("low-memory"));
        assert!(ProcessStatus::Exited(0).explain().contains("cleanly"));
    }

    #[test]
    fn args_are_kept_as_separate_elements() {
        // A URI containing shell metacharacters must survive as one argument.
        let spec =
            ProcessSpec::new("wine", "/usr/bin/wine").arg("vortex://play?game=1&token=a;rm -rf /");
        assert_eq!(spec.args.len(), 1);
        assert!(spec.args[0].contains(';'));
    }
}

//! Static registry for the terminal adapters shipped in this binary.
//!
//! This is intentionally not a dynamic plugin loader. It centralizes adapter
//! construction and presentation defaults while the serialized `sessions`
//! array remains backward compatible.

use std::borrow::Cow;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Context as _;

use super::{BackendKind, HerdrBackend, HerdrCoreBackend, TerminalBackend, TmuxBackend};

pub struct BackendRegistry {
    herdr_default_socket: String,
}

impl BackendRegistry {
    pub fn new(herdr_default_socket: impl Into<String>) -> Self {
        Self {
            herdr_default_socket: herdr_default_socket.into(),
        }
    }

    pub fn connect(
        &self,
        kind: BackendKind,
        socket_path: &str,
        core_command: &[String],
    ) -> Box<dyn TerminalBackend> {
        match kind {
            BackendKind::Herdr => Box::new(HerdrBackend::new(socket_path)),
            BackendKind::HerdrCore => {
                Box::new(HerdrCoreBackend::with_command(socket_path, core_command))
            }
            BackendKind::Tmux => Box::new(TmuxBackend::new(
                (!socket_path.is_empty()).then(|| PathBuf::from(socket_path)),
            )),
        }
    }

    pub fn default_socket(&self, kind: BackendKind) -> String {
        match kind {
            BackendKind::Herdr => self.herdr_default_socket.clone(),
            BackendKind::HerdrCore => native_core_socket(),
            BackendKind::Tmux => String::new(),
        }
    }

    pub fn default_label(&self, kind: BackendKind) -> &'static str {
        match kind {
            BackendKind::Herdr => "Herdr",
            BackendKind::HerdrCore => "Herdr core",
            BackendKind::Tmux => "tmux",
        }
    }

    pub fn endpoint<'a>(&self, kind: BackendKind, socket_path: &'a str) -> Cow<'a, str> {
        match kind {
            BackendKind::Tmux if socket_path.is_empty() => Cow::Borrowed("default tmux server"),
            _ => Cow::Borrowed(socket_path),
        }
    }

    pub fn ensure_available(&self, kind: BackendKind) -> anyhow::Result<()> {
        match kind {
            BackendKind::Herdr | BackendKind::HerdrCore => Ok(()),
            BackendKind::Tmux => {
                // Resolved rather than spawned by name, so that a failure here
                // can say which PATH was searched. This check runs in the
                // installer's shell and the gateway may later run under an init
                // system with a narrower one; naming the PATH is what makes the
                // two environments distinguishable to whoever reads the error.
                let path = std::env::var("PATH").unwrap_or_default();
                let program =
                    crate::login_env::lookup(super::TMUX_PROGRAM, &path).with_context(|| {
                        format!("tmux backend selected, but no `tmux` was found on PATH={path}")
                    })?;
                let output = Command::new(&program).arg("-V").output().with_context(|| {
                    format!(
                        "tmux backend selected, but {} would not run",
                        program.display()
                    )
                })?;
                anyhow::ensure!(
                    output.status.success(),
                    "tmux backend selected, but `tmux -V` failed"
                );
                Ok(())
            }
        }
    }
}

fn native_core_socket() -> String {
    if let Some(socket) = std::env::var_os("HC_SOCKET").filter(|v| !v.is_empty()) {
        return socket.to_string_lossy().into_owned();
    }
    #[cfg(unix)]
    {
        let run = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                // SAFETY: getuid has no preconditions and does not mutate process state.
                let path = PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }));
                path.is_dir().then_some(path)
            });
        run.map(|p| p.join("herdr-core"))
            .unwrap_or_else(|| {
                std::env::var_os("XDG_STATE_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/share"))
                    .join("herdr-core/default")
            })
            .join("default.sock")
            .to_string_lossy()
            .into_owned()
    }
    #[cfg(windows)]
    {
        let user: String = std::env::var("USERNAME")
            .unwrap_or_default()
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        format!(r"\\.\pipe\herdr-core-{user}-default.sock")
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_defaults_live_with_the_registered_adapter() {
        let registry = BackendRegistry::new("/run/herdr.sock");
        assert_eq!(registry.default_label(BackendKind::Herdr), "Herdr");
        assert_eq!(
            registry.default_socket(BackendKind::Herdr),
            "/run/herdr.sock"
        );
        assert_eq!(
            registry.endpoint(BackendKind::Tmux, ""),
            "default tmux server"
        );
    }
}

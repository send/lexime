use std::path::Path;
use std::sync::Arc;

use crate::user_dict::{UserDictOpenReport, UserDictionary};

use super::{LexError, LexUserWord};

#[derive(uniffi::Object)]
pub struct LexUserDictionary {
    pub(crate) inner: Arc<UserDictionary>,
    report: UserDictOpenReport,
}

/// What `open` found and did. `data_loss_suspected` is computed on the Rust
/// side so Swift stays presentational (mirrors `LexHistoryOpenReport`).
#[derive(uniffi::Record)]
pub struct LexUserDictOpenReport {
    /// Display-only paths where corrupt bytes were preserved (`.corrupt-<ts>`).
    pub quarantined_paths: Vec<String>,
    /// Registered words were lost: the file was corrupt and quarantined.
    pub data_loss_suspected: bool,
}

impl From<&UserDictOpenReport> for LexUserDictOpenReport {
    fn from(r: &UserDictOpenReport) -> Self {
        Self {
            // Lossy on purpose: display-only, and a non-UTF-8 path must not
            // panic at the FFI boundary.
            quarantined_paths: r
                .quarantined_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            data_loss_suspected: r.data_loss_suspected(),
        }
    }
}

#[uniffi::export]
impl LexUserDictionary {
    #[uniffi::constructor]
    fn open(path: String) -> Result<Arc<Self>, LexError> {
        // Recovery-mode open: corruption is quarantined instead of failing, so
        // a corrupt file never permanently disables user-word registration.
        // Err = environmental read failure only (EACCES etc.). Swift consumes
        // the report via `open_report()`; the log here covers headless/CLI.
        let (dict, report) = UserDictionary::open_recovering(Path::new(&path))?;
        if report.data_loss_suspected() {
            tracing::warn!(
                "user dictionary recovered with data loss (quarantined: {:?})",
                report.quarantined_paths
            );
        }
        Ok(Arc::new(Self {
            inner: Arc::new(dict),
            report,
        }))
    }

    /// What recovery found and did at open time (data loss on quarantine).
    fn open_report(&self) -> LexUserDictOpenReport {
        (&self.report).into()
    }

    fn register(&self, reading: String, surface: String) -> bool {
        self.inner.register(&reading, &surface)
    }

    fn unregister(&self, reading: String, surface: String) -> bool {
        self.inner.unregister(&reading, &surface)
    }

    fn list(&self) -> Vec<LexUserWord> {
        self.inner
            .list()
            .into_iter()
            .map(|(reading, surface)| LexUserWord { reading, surface })
            .collect()
    }

    fn save(&self, path: String) -> Result<(), LexError> {
        self.inner.save(Path::new(&path))?;
        Ok(())
    }
}

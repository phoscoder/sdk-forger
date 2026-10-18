//! Headless Generate, Maintain and Port pipelines.
//!
//! The engine never prints or reads the terminal. Progress goes out as [`Event`]s and the one
//! interactive decision (approving a diff) goes through [`Host::approve`], so the CLI and the
//! desktop app drive the same code.

mod deliver;
mod generate;
mod maintain;
mod port;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Mutex;

use sdkforge_generate::GeneratedFile;
use sdkforge_verify::SANDBOX_ARTIFACTS;

pub use deliver::{deliver, DeliverRequest, Package};
pub use generate::{generate, Delivery, GenerateRequest, LangResult};
pub use maintain::{maintain, MaintainRequest};
pub use port::{port, PortRequest};

/// Anything that went wrong in a pipeline, as a message for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineError(pub String);

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EngineError {}

impl From<String> for EngineError {
    fn from(s: String) -> Self {
        EngineError(s)
    }
}

impl From<&str> for EngineError {
    fn from(s: &str) -> Self {
        EngineError(s.to_string())
    }
}

pub type Result<T> = std::result::Result<T, EngineError>;

/// Convert any displayable error into an [`EngineError`].
pub(crate) trait Ctx<T> {
    fn ctx(self) -> Result<T>;
}

impl<T, E: fmt::Display> Ctx<T> for std::result::Result<T, E> {
    fn ctx(self) -> Result<T> {
        self.map_err(|e| EngineError(e.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    /// Raw command output or other detail meant for a log pane, not a status line.
    Detail,
}

/// One progress message. `scope` names the language or stage it belongs to (empty if none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub scope: String,
    pub level: Level,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Approve,
    Reject,
    Feedback(String),
}

/// What the caller provides: somewhere to send events and a way to ask for approval.
pub trait Host: Sync {
    fn event(&self, event: Event);
    /// Show a verified diff and get the user's decision.
    fn approve(&self, diff: &str) -> Verdict;
    /// Ask before verified files are written to their destination. `listing` names the files.
    fn approve_files(&self, listing: &str) -> bool {
        self.approve(listing) == Verdict::Approve
    }
}

/// Scoped logging helper used by the pipelines.
pub(crate) struct Log<'a> {
    host: &'a dyn Host,
    scope: String,
}

impl<'a> Log<'a> {
    pub fn new(host: &'a dyn Host, scope: &str) -> Self {
        Log {
            host,
            scope: scope.to_string(),
        }
    }

    pub fn host(&self) -> &'a dyn Host {
        self.host
    }

    fn send(&self, level: Level, message: impl Into<String>) {
        self.host.event(Event {
            scope: self.scope.clone(),
            level,
            message: message.into(),
        });
    }

    pub fn info(&self, m: impl Into<String>) {
        self.send(Level::Info, m);
    }

    pub fn warn(&self, m: impl Into<String>) {
        self.send(Level::Warn, m);
    }

    pub fn detail(&self, m: impl Into<String>) {
        self.send(Level::Detail, m);
    }
}

/// A [`Host`] that forwards events over a channel and asks for approvals over another, for a UI
/// running on a different thread. The UI answers each [`ApprovalRequest`] on its `reply` sender.
pub struct ChannelHost {
    events: Mutex<Sender<Event>>,
    approvals: Mutex<Sender<ApprovalRequest>>,
}

pub struct ApprovalRequest {
    pub diff: String,
    pub reply: Sender<Verdict>,
}

impl ChannelHost {
    pub fn new() -> (Self, Receiver<Event>, Receiver<ApprovalRequest>) {
        let (etx, erx) = std::sync::mpsc::channel();
        let (atx, arx) = std::sync::mpsc::channel();
        (
            ChannelHost {
                events: Mutex::new(etx),
                approvals: Mutex::new(atx),
            },
            erx,
            arx,
        )
    }
}

impl Host for ChannelHost {
    fn event(&self, event: Event) {
        if let Ok(tx) = self.events.lock() {
            tx.send(event).ok();
        }
    }

    fn approve(&self, diff: &str) -> Verdict {
        let (reply, answer) = std::sync::mpsc::channel();
        let sent = self.approvals.lock().is_ok_and(|tx| {
            tx.send(ApprovalRequest {
                diff: diff.to_string(),
                reply,
            })
            .is_ok()
        });
        if !sent {
            return Verdict::Reject;
        }
        answer.recv().unwrap_or(Verdict::Reject)
    }
}

/// A scratch directory that is removed when dropped. Pipelines verify here when the user wants to
/// approve files before anything reaches the real output directory.
pub(crate) struct Staging(PathBuf);

impl Staging {
    pub fn new(tag: &str) -> Result<Self> {
        let dir = std::env::temp_dir().join(format!(
            "sdkforge-stage-{}-{}",
            std::process::id(),
            tag.replace(['/', '\\'], "_")
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).ctx()?;
        Ok(Staging(dir))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn skipped(name: &str) -> bool {
    name == ".git" || SANDBOX_ARTIFACTS.contains(&name)
}

/// Relative paths of the files under `dir`, sorted, without sandbox artifacts.
pub(crate) fn list_files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            if skipped(&e.file_name().to_string_lossy()) {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(rel) = p.strip_prefix(dir) {
                out.push(rel.to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out
}

/// Copy the files `list_files` reports from one directory into another.
pub(crate) fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    for rel in list_files(from) {
        let dest = to.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).ctx()?;
        }
        std::fs::copy(from.join(&rel), dest).ctx()?;
    }
    Ok(())
}

/// Ask the host whether the verified files in `stage` may be written to `dest`.
pub(crate) fn approve_and_copy(host: &dyn Host, stage: &Path, dest: &Path) -> Result<()> {
    let files = list_files(stage);
    let listing = format!(
        "Write {} file(s) to {}:\n{}",
        files.len(),
        dest.display(),
        files.join("\n")
    );
    if !host.approve_files(&listing) {
        return Err("rejected; nothing written".into());
    }
    copy_tree(stage, dest)
}

pub(crate) fn write_files(root: &Path, files: &[GeneratedFile]) -> Result<()> {
    for f in files {
        let p = root.join(&f.path);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).ctx()?;
        }
        std::fs::write(&p, &f.content).ctx()?;
    }
    Ok(())
}

/// Later files replace earlier ones with the same path.
pub(crate) fn merge(all: &mut Vec<GeneratedFile>, new: Vec<GeneratedFile>) {
    for f in new {
        match all.iter_mut().find(|x| x.path == f.path) {
            Some(x) => *x = f,
            None => all.push(f),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_host_forwards_events_and_approvals() {
        let (host, events, approvals) = ChannelHost::new();
        Log::new(&host, "python").info("hello");
        let e = events.recv().unwrap();
        assert_eq!((e.scope.as_str(), e.message.as_str()), ("python", "hello"));

        let ui = std::thread::spawn(move || {
            let req = approvals.recv().unwrap();
            assert_eq!(req.diff, "d");
            req.reply.send(Verdict::Feedback("shorter".into())).unwrap();
        });
        assert_eq!(host.approve("d"), Verdict::Feedback("shorter".into()));
        ui.join().unwrap();
    }

    #[test]
    fn dropped_ui_rejects() {
        let (host, _events, approvals) = ChannelHost::new();
        drop(approvals);
        assert_eq!(host.approve("d"), Verdict::Reject);
    }

    struct Answer(bool);
    impl Host for Answer {
        fn event(&self, _: Event) {}
        fn approve(&self, _: &str) -> Verdict {
            if self.0 {
                Verdict::Approve
            } else {
                Verdict::Reject
            }
        }
    }

    #[test]
    fn approval_gates_the_copy_and_skips_sandbox_artifacts() {
        let stage = Staging::new("test-approval").unwrap();
        std::fs::create_dir_all(stage.path().join("pkg")).unwrap();
        std::fs::create_dir_all(stage.path().join(".deps")).unwrap();
        std::fs::write(stage.path().join("pkg/a.py"), "a").unwrap();
        std::fs::write(stage.path().join(".deps/x"), "x").unwrap();
        let dest = std::env::temp_dir().join(format!("sdkforge-dest-{}", std::process::id()));

        assert!(approve_and_copy(&Answer(false), stage.path(), &dest).is_err());
        assert!(!dest.exists());

        approve_and_copy(&Answer(true), stage.path(), &dest).unwrap();
        assert!(dest.join("pkg/a.py").exists());
        assert!(!dest.join(".deps").exists());
        std::fs::remove_dir_all(&dest).ok();
    }

    #[test]
    fn merge_replaces_by_path() {
        let f = |p: &str, c: &str| GeneratedFile {
            path: p.into(),
            content: c.into(),
        };
        let mut all = vec![f("a", "1"), f("b", "1")];
        merge(&mut all, vec![f("b", "2"), f("c", "3")]);
        assert_eq!(all.len(), 3);
        assert_eq!(all[1].content, "2");
    }
}

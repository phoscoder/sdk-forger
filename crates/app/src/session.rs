//! Bridge between the UI thread and an engine run on its own thread.
//!
//! The UI never calls the engine directly: it starts a [`Session`], then drains its channels on a
//! timer. The engine sees a [`ChannelHost`], so it neither prints nor blocks the UI.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;

use sdkforge_engine::{
    ApprovalRequest, ChannelHost, Event, GenerateRequest, Level, MaintainRequest, PortRequest,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Generate,
    Maintain,
    Port,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Generate, Mode::Maintain, Mode::Port];

    pub fn label(self) -> &'static str {
        match self {
            Mode::Generate => "Generate",
            Mode::Maintain => "Maintain",
            Mode::Port => "Port",
        }
    }
}

/// What the user filled in on the Project and Instructions screens.
#[derive(Clone, Default)]
pub struct Inputs {
    /// Spec path (Generate) or repo URL/path (Maintain, Port).
    pub source: String,
    /// Target languages, comma-separated. Port takes the first one.
    pub langs: String,
    pub out: String,
    pub instructions: String,
    /// Maintain: the error or improvement to make.
    pub request: String,
}

#[derive(Default)]
pub struct Lane {
    pub stage: String,
    pub lines: Vec<(Level, String)>,
}

#[derive(Default)]
pub struct Outcome {
    pub ok: bool,
    pub text: String,
}

pub struct Session {
    events: Receiver<Event>,
    approvals: Receiver<ApprovalRequest>,
    done: Receiver<Vec<(String, Outcome)>>,
    /// One lane per language (or "" for single-scope runs), in first-seen order of the name.
    pub lanes: BTreeMap<String, Lane>,
    pub pending: Option<ApprovalRequest>,
    pub finished: Option<Vec<(String, Outcome)>>,
}

fn lang_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

impl Session {
    pub fn start(mode: Mode, i: &Inputs) -> Session {
        let (host, events, approvals) = ChannelHost::new();
        let host = Arc::new(host);
        let (done_tx, done) = std::sync::mpsc::channel();
        let i = i.clone();
        std::thread::spawn(move || {
            let h = host.as_ref();
            let one = |r: sdkforge_engine::Result<String>| {
                vec![(
                    String::new(),
                    match r {
                        Ok(text) => Outcome { ok: true, text },
                        Err(e) => Outcome {
                            ok: false,
                            text: e.0,
                        },
                    },
                )]
            };
            let results = match mode {
                Mode::Generate => {
                    let req = GenerateRequest {
                        spec: i.source,
                        instructions: i.instructions,
                        langs: lang_list(&i.langs),
                        out: PathBuf::from(if i.out.is_empty() { "out" } else { &i.out }),
                        dry_run: false,
                        model: None,
                        max_attempts: 5,
                        token_budget: 500_000,
                        deliver: None,
                        require_approval: false,
                    };
                    match sdkforge_engine::generate(h, &req) {
                        Ok(rs) => rs
                            .into_iter()
                            .map(|r| {
                                let o = match r.result {
                                    Ok(text) => Outcome { ok: true, text },
                                    Err(text) => Outcome { ok: false, text },
                                };
                                (r.lang.name().to_string(), o)
                            })
                            .collect(),
                        Err(e) => one(Err(e)),
                    }
                }
                Mode::Maintain => one(sdkforge_engine::maintain(
                    h,
                    &MaintainRequest {
                        repo: i.source,
                        request: i.request,
                        bug: false,
                        workdir: None,
                        push: false,
                        model: None,
                        max_attempts: 5,
                        token_budget: 500_000,
                    },
                )),
                Mode::Port => one(sdkforge_engine::port(
                    h,
                    &PortRequest {
                        repo: i.source,
                        to: lang_list(&i.langs).into_iter().next().unwrap_or_default(),
                        from: None,
                        instructions: i.instructions,
                        out: PathBuf::from(if i.out.is_empty() { "port-out" } else { &i.out }),
                        model: None,
                        max_attempts: 5,
                        token_budget: 800_000,
                        parity: true,
                        require_approval: false,
                        accept: Vec::new(),
                    },
                )),
            };
            done_tx.send(results).ok();
        });
        Session {
            events,
            approvals,
            done,
            lanes: BTreeMap::new(),
            pending: None,
            finished: None,
        }
    }

    /// Move everything the engine has produced since the last call into the UI state.
    /// Returns true if anything changed.
    pub fn drain(&mut self) -> bool {
        let mut changed = false;
        while let Ok(e) = self.events.try_recv() {
            let lane = self.lanes.entry(e.scope).or_default();
            if e.level == Level::Info {
                lane.stage = e.message.clone();
            }
            lane.lines.push((e.level, e.message));
            changed = true;
        }
        if self.pending.is_none() {
            if let Ok(req) = self.approvals.try_recv() {
                self.pending = Some(req);
                changed = true;
            }
        }
        match self.done.try_recv() {
            Ok(results) => {
                self.finished = Some(results);
                changed = true;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                if self.finished.is_none() {
                    self.finished = Some(vec![(
                        String::new(),
                        Outcome {
                            ok: false,
                            text: "the engine thread stopped unexpectedly".into(),
                        },
                    )]);
                    changed = true;
                }
            }
        }
        changed
    }
}

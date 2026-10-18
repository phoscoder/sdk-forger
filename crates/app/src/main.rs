//! GPUI desktop front end. The UI thread never calls the engine directly: a [`Session`] runs the
//! pipeline on its own thread and the UI drains its channels on a timer.

mod session;

use std::time::Duration;

use gpui_kit::base::input::{InputState, TextareaState};
use gpui_kit::rems;
use gpui_kit::{
    div, prelude::*, App, Context, Entity, FontWeight, SharedString, Window, WindowOptions,
};
use gpui_omarchy::*;
use sdkforge_engine::{Level, Verdict};
use session::{Inputs, Mode, Session};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Project,
    Instructions,
    Run,
    Review,
    Output,
}

impl Screen {
    const ALL: [Screen; 5] = [
        Screen::Project,
        Screen::Instructions,
        Screen::Run,
        Screen::Review,
        Screen::Output,
    ];

    fn label(self) -> &'static str {
        match self {
            Screen::Project => "Project",
            Screen::Instructions => "Instructions",
            Screen::Run => "Run",
            Screen::Review => "Diff review",
            Screen::Output => "Output",
        }
    }
}

struct Forge {
    screen: Screen,
    mode: Mode,
    source: Entity<InputState>,
    langs: Entity<InputState>,
    out: Entity<InputState>,
    request: Entity<InputState>,
    feedback: Entity<InputState>,
    instructions: Entity<TextareaState>,
    session: Option<Session>,
}

impl Forge {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let field = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let this = Forge {
            screen: Screen::Project,
            mode: Mode::Generate,
            source: field(
                "Spec file or folder (Generate), repo URL or path (Maintain, Port)",
                window,
                cx,
            ),
            langs: field("python, node, php, rust, go (Port: the target)", window, cx),
            out: field("Output directory", window, cx),
            request: field("Maintain: the error message or improvement", window, cx),
            feedback: field("Feedback for another round", window, cx),
            instructions: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .rows(10)
                    .placeholder("Instructions for the model, e.g. naming, error handling, style")
            }),
            session: None,
        };
        // Drain the engine's channels ten times a second.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            let alive = this.update(cx, |this, cx| {
                let changed = this.session.as_mut().is_some_and(|s| s.drain());
                if changed {
                    if this.session.as_ref().is_some_and(|s| s.pending.is_some()) {
                        this.screen = Screen::Review;
                    }
                    cx.notify();
                }
            });
            if alive.is_err() {
                break;
            }
        })
        .detach();
        this
    }

    fn inputs(&self, cx: &App) -> Inputs {
        let text = |e: &Entity<InputState>| e.read(cx).value().to_string();
        Inputs {
            source: text(&self.source),
            langs: text(&self.langs),
            out: text(&self.out),
            instructions: self.instructions.read(cx).value().to_string(),
            request: text(&self.request),
        }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let inputs = self.inputs(cx);
        self.session = Some(Session::start(self.mode, &inputs));
        self.screen = Screen::Run;
        cx.notify();
    }

    fn answer(&mut self, verdict: Verdict, cx: &mut Context<Self>) {
        if let Some(req) = self.session.as_mut().and_then(|s| s.pending.take()) {
            req.reply.send(verdict).ok();
        }
        self.screen = Screen::Run;
        cx.notify();
    }

    fn running(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.finished.is_none())
    }

    fn nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div().flex().flex_col().gap_1().w(rems(11.)).p_2();
        for screen in Screen::ALL {
            let variant = if screen == self.screen {
                ButtonVariant::Primary
            } else {
                ButtonVariant::Secondary
            };
            col = col.child(
                button(screen.label(), screen.label(), variant, cx).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.screen = screen;
                        cx.notify();
                    },
                )),
            );
        }
        col
    }

    fn project(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut modes = tabs("mode", cx);
        for mode in Mode::ALL {
            modes = modes.child(
                tab(mode.label(), mode.label(), mode == self.mode, cx).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.mode = mode;
                        cx.notify();
                    },
                )),
            );
        }
        let mut p = panel("Project", cx)
            .child(modes)
            .child(input("source", &self.source, window, cx))
            .child(input("langs", &self.langs, window, cx))
            .child(input("out", &self.out, window, cx));
        if self.mode == Mode::Maintain {
            p = p.child(input("request", &self.request, window, cx));
        }
        p.child(
            button("start", self.mode.label(), ButtonVariant::Primary, cx)
                .disabled(self.running())
                .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
        )
    }

    fn instructions(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        panel("Instructions", cx)
            .child("Sent with every model request for Generate and Port.")
            .child(textarea("instructions", &self.instructions, window, cx))
    }

    fn run(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.omarchy().clone();
        let Some(session) = &self.session else {
            return panel("Run", cx).child("Nothing running. Start one from the Project screen.");
        };
        let mut grid = div().flex().flex_wrap().gap_3();
        for (scope, lane) in &session.lanes {
            let title = if scope.is_empty() { "run" } else { scope };
            let mut log = div()
                .id(SharedString::from(format!("log-{title}")))
                .flex()
                .flex_col()
                .h(rems(16.))
                .overflow_y_scroll()
                .font_family(t.mono_font.clone())
                .text_size(rems(0.6875));
            for (level, line) in lane.lines.iter().rev().take(200).rev() {
                let color = match level {
                    Level::Warn => t.warning,
                    Level::Detail => t.secondary,
                    Level::Info => t.foreground,
                };
                log = log.child(div().text_color(color).child(line.clone()));
            }
            grid = grid.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .w(rems(26.))
                    .p_2()
                    .border_1()
                    .border_color(t.border)
                    .child(div().font_weight(FontWeight::BOLD).child(title.to_string()))
                    .child(div().text_color(t.accent).child(lane.stage.clone()))
                    .child(log),
            );
        }
        let status = match &session.finished {
            None => "running...".to_string(),
            Some(_) => "finished, see Output".to_string(),
        };
        panel("Run", cx).child(status).child(grid)
    }

    fn review(&self, cx: &mut Context<Self>, window: &Window) -> impl IntoElement {
        let t = cx.omarchy().clone();
        let Some(req) = self.session.as_ref().and_then(|s| s.pending.as_ref()) else {
            return panel("Diff review", cx).child("No diff is waiting for approval.");
        };
        let mut diff = div()
            .id("diff")
            .max_h(rems(28.))
            .overflow_y_scroll()
            .font_family(t.mono_font.clone())
            .text_size(rems(0.6875));
        for line in req.diff.lines() {
            let color = match line.chars().next() {
                Some('+') => t.success,
                Some('-') => t.danger,
                Some('@') => t.accent,
                _ => t.foreground,
            };
            diff = diff.child(div().text_color(color).child(line.to_string()));
        }
        panel("Diff review", cx)
            .child("Tests pass in the sandbox. Approve to commit on a new branch.")
            .child(diff)
            .child(input("feedback", &self.feedback, window, cx))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button("approve", "Approve", ButtonVariant::Primary, cx).on_click(
                            cx.listener(|this, _, _, cx| this.answer(Verdict::Approve, cx)),
                        ),
                    )
                    .child(
                        button(
                            "feedback-send",
                            "Send feedback",
                            ButtonVariant::Secondary,
                            cx,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            let text = this.feedback.read(cx).value().to_string();
                            if !text.trim().is_empty() {
                                this.feedback
                                    .update(cx, |f, cx| f.set_value("", window, cx));
                                this.answer(Verdict::Feedback(text), cx);
                            }
                        })),
                    )
                    .child(
                        button("reject", "Reject", ButtonVariant::Danger, cx).on_click(
                            cx.listener(|this, _, _, cx| this.answer(Verdict::Reject, cx)),
                        ),
                    ),
            )
    }

    fn output(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.omarchy().clone();
        let mut p = panel("Output", cx);
        match self.session.as_ref().and_then(|s| s.finished.as_ref()) {
            None => p = p.child("No finished run yet."),
            Some(results) => {
                for (lang, outcome) in results {
                    let name = if lang.is_empty() { "result" } else { lang };
                    p = p.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(if outcome.ok { t.success } else { t.danger })
                                    .child(format!(
                                        "{name}: {}",
                                        if outcome.ok { "ok" } else { "failed" }
                                    )),
                            )
                            .child(outcome.text.clone()),
                    );
                }
            }
        }
        p
    }
}

impl Render for Forge {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.omarchy().clone();
        let body = match self.screen {
            Screen::Project => self.project(window, cx).into_any_element(),
            Screen::Instructions => self.instructions(window, cx).into_any_element(),
            Screen::Run => self.run(cx).into_any_element(),
            Screen::Review => self.review(cx, window).into_any_element(),
            Screen::Output => self.output(cx).into_any_element(),
        };
        focus_scope("forge")
            .size_full()
            .flex()
            .bg(t.background)
            .text_color(t.foreground)
            .font_family(t.font.clone())
            .child(self.nav(cx))
            .child(
                div()
                    .id("content")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .p_3()
                    .child(body),
            )
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_omarchy::init(cx);
            cx.open_window(WindowOptions::default(), |window, cx| {
                cx.new(|cx| Forge::new(window, cx))
            })
            .expect("open window");
            cx.activate(true);
        });
}

//! GPUI desktop front end. The UI thread never calls the engine directly: a [`Session`] runs the
//! pipeline on its own thread and the UI drains its channels on a timer.

mod session;

use std::time::Duration;

use gpui_kit::base::input::{InputState, TextareaState};
use gpui_kit::rems;
use gpui_kit::{
    div, prelude::*, AnyElement, App, Context, Div, Entity, FontWeight, SharedString, Window,
    WindowOptions,
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
                "petstore.yaml, ./collection, or https://github.com/acme/sdk",
                window,
                cx,
            ),
            langs: field("python, node", window, cx),
            out: field("out", window, cx),
            request: field("paginate stops early on an empty cursor", window, cx),
            feedback: field("e.g. keep the public method names unchanged", window, cx),
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

    fn status(&self) -> (&'static str, Status) {
        match self.session.as_ref() {
            None => ("Idle", Status::Neutral),
            Some(s) if s.pending.is_some() => ("Needs review", Status::Warning),
            Some(s) => match &s.finished {
                None => ("Running", Status::Warning),
                Some(r) if r.iter().all(|(_, o)| o.ok) => ("Done", Status::Success),
                Some(_) => ("Failed", Status::Error),
            },
        }
    }

    fn pill(&self, label: &str, status: Status, cx: &App) -> Div {
        let c = status.color(cx);
        div()
            .flex()
            .items_center()
            .gap_1p5()
            .px_2p5()
            .py_0p5()
            .rounded_full()
            .bg(c.opacity(0.14))
            .text_color(c)
            .text_size(rems(0.6875))
            .font_weight(FontWeight::SEMIBOLD)
            .child(div().size(rems(0.375)).rounded_full().bg(c))
            .child(label.to_string())
    }

    fn nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.omarchy().clone();
        let (status_label, status) = self.status();
        let waiting = self.session.as_ref().is_some_and(|s| s.pending.is_some());
        let mut items = div().flex().flex_col().gap_1().px_3();
        for (i, screen) in Screen::ALL.into_iter().enumerate() {
            let selected = screen == self.screen;
            let badge_dot = screen == Screen::Review && waiting;
            items = items.child(
                div()
                    .id(screen.label())
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .cursor_pointer()
                    .text_size(rems(0.8125))
                    .when(selected, |d| {
                        d.bg(t.accent.opacity(0.14))
                            .text_color(t.accent)
                            .font_weight(FontWeight::SEMIBOLD)
                    })
                    .when(!selected, |d| {
                        d.text_color(t.secondary)
                            .hover(|h| h.bg(t.foreground.opacity(0.06)).text_color(t.foreground))
                    })
                    .child(
                        div()
                            .size(rems(1.375))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .text_size(rems(0.6875))
                            .font_weight(FontWeight::BOLD)
                            .when(selected, |d| d.bg(t.accent).text_color(t.on_accent))
                            .when(!selected, |d| d.bg(t.foreground.opacity(0.08)))
                            .child((i + 1).to_string()),
                    )
                    .child(div().flex_1().child(screen.label()))
                    .when(badge_dot, |d| {
                        d.child(div().size(rems(0.5)).rounded_full().bg(t.warning))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.screen = screen;
                        cx.notify();
                    })),
            );
        }
        div()
            .flex()
            .flex_col()
            .justify_between()
            .w(rems(15.))
            .h_full()
            .flex_shrink_0()
            .py_5()
            .bg(t.surface)
            .border_r_1()
            .border_color(t.border)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_6()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .px_6()
                            .child(
                                div()
                                    .size(rems(2.25))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_xl()
                                    .bg(t.accent)
                                    .text_color(t.on_accent)
                                    .font_weight(FontWeight::BOLD)
                                    .child("SF"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .text_size(rems(0.9375))
                                            .font_weight(FontWeight::BOLD)
                                            .child("SDK Forge"),
                                    )
                                    .child(
                                        div()
                                            .text_size(rems(0.6875))
                                            .text_color(t.secondary)
                                            .child("Verified SDKs"),
                                    ),
                            ),
                    )
                    .child(items),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .px_6()
                    .child(
                        div()
                            .text_size(rems(0.6875))
                            .text_color(t.secondary)
                            .child("STATUS"),
                    )
                    .child(self.pill(status_label, status, cx)),
            )
    }

    /// Page frame: title, one-line explanation, then the body in a centered column.
    fn page(
        &self,
        title: &str,
        subtitle: &str,
        right: Option<AnyElement>,
        body: impl IntoElement,
        cx: &App,
    ) -> Div {
        let t = cx.omarchy();
        div()
            .flex()
            .flex_col()
            .gap_6()
            .w_full()
            .min_w_0()
            .max_w(rems(58.))
            .mx_auto()
            .child(
                div()
                    .flex()
                    .items_end()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(rems(1.625))
                                    .font_weight(FontWeight::BOLD)
                                    .child(title.to_string()),
                            )
                            .child(
                                div()
                                    .text_size(rems(0.875))
                                    .text_color(t.secondary)
                                    .child(subtitle.to_string()),
                            ),
                    )
                    .children(right),
            )
            .child(body)
    }

    fn card(&self, cx: &App) -> Div {
        let t = cx.omarchy();
        div()
            .flex()
            .flex_col()
            .gap_4()
            .p_5()
            .rounded_xl()
            .border_1()
            .border_color(t.border)
            .bg(t.surface)
    }

    fn field(&self, label: &str, hint: &str, control: impl IntoElement, cx: &App) -> Div {
        let t = cx.omarchy();
        div()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(
                div()
                    .text_size(rems(0.75))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(label.to_string()),
            )
            .child(control)
            .child(
                div()
                    .text_size(rems(0.6875))
                    .text_color(t.secondary)
                    .child(hint.to_string()),
            )
    }

    fn empty(&self, title: &str, text: &str, cx: &App) -> Div {
        let t = cx.omarchy();
        self.card(cx)
            .items_center()
            .py_12()
            .child(
                div()
                    .size(rems(3.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(t.accent.opacity(0.14))
                    .text_color(t.accent)
                    .text_size(rems(1.25))
                    .child("◇"),
            )
            .child(
                div()
                    .text_size(rems(1.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title.to_string()),
            )
            .child(
                div()
                    .text_size(rems(0.8125))
                    .text_color(t.secondary)
                    .child(text.to_string()),
            )
    }

    fn project(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.omarchy().clone();
        let blurb = |m: Mode| match m {
            Mode::Generate => "Build a new SDK from an OpenAPI, Postman or Bruno spec.",
            Mode::Maintain => "Fix a bug or improve an existing SDK repo with a reviewed patch.",
            Mode::Port => "Rebuild an SDK in another language, checked against the original.",
        };
        let mut modes = div().flex().gap_3();
        for mode in Mode::ALL {
            let selected = mode == self.mode;
            modes = modes.child(
                div()
                    .id(mode.label())
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_4()
                    .rounded_xl()
                    .border_1()
                    .cursor_pointer()
                    .when(selected, |d| {
                        d.border_color(t.accent).bg(t.accent.opacity(0.10))
                    })
                    .when(!selected, |d| {
                        d.border_color(t.border)
                            .bg(t.surface)
                            .hover(|h| h.border_color(t.foreground.opacity(0.35)))
                    })
                    .child(
                        div()
                            .text_size(rems(0.9375))
                            .font_weight(FontWeight::BOLD)
                            .when(selected, |d| d.text_color(t.accent))
                            .child(mode.label()),
                    )
                    .child(
                        div()
                            .text_size(rems(0.75))
                            .text_color(t.secondary)
                            .child(blurb(mode)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.mode = mode;
                        cx.notify();
                    })),
            );
        }
        let (source_label, source_hint) = match self.mode {
            Mode::Generate => (
                "API description",
                "Path to an OpenAPI file, Postman collection or Bruno folder.",
            ),
            _ => ("Repository", "A git URL or a local path to the SDK."),
        };
        let lang_label = if self.mode == Mode::Port {
            "Target language"
        } else {
            "Languages"
        };
        let lang_hint = if self.mode == Mode::Port {
            "One of python, node, php, rust, go."
        } else {
            "Comma-separated: python, node, php, rust, go. Several run in parallel."
        };
        let mut form = self.card(cx).child(self.field(
            source_label,
            source_hint,
            input("source", &self.source, window, cx),
            cx,
        ));
        if self.mode == Mode::Maintain {
            form = form.child(self.field(
                "What should change?",
                "Paste the error message or describe the improvement.",
                input("request", &self.request, window, cx),
                cx,
            ));
        } else {
            form = form
                .child(self.field(
                    lang_label,
                    lang_hint,
                    input("langs", &self.langs, window, cx),
                    cx,
                ))
                .child(self.field(
                    "Output folder",
                    "Where the verified SDK is written.",
                    input("out", &self.out, window, cx),
                    cx,
                ));
        }
        let running = self.running();
        let go = div()
            .id("start")
            .flex()
            .items_center()
            .justify_center()
            .px_6()
            .py_2p5()
            .rounded_lg()
            .text_size(rems(0.875))
            .font_weight(FontWeight::BOLD)
            .when(running, |d| {
                d.bg(t.foreground.opacity(0.10)).text_color(t.secondary)
            })
            .when(!running, |d| {
                d.bg(t.accent)
                    .text_color(t.on_accent)
                    .cursor_pointer()
                    .hover(|h| h.opacity(0.88))
                    .on_click(cx.listener(|this, _, _, cx| this.start(cx)))
            })
            .child(if running {
                "Working…".to_string()
            } else {
                format!("Start {}", self.mode.label().to_lowercase())
            });
        self.page(
            "Project",
            "Choose what to do, then point it at your input.",
            None,
            div()
                .flex()
                .flex_col()
                .gap_5()
                .child(modes)
                .child(form)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(rems(0.75))
                                .text_color(t.secondary)
                                .child("Every build runs in a Docker sandbox before you see it."),
                        )
                        .child(go),
                ),
            cx,
        )
    }

    fn instructions(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self
            .card(cx)
            .child(self.field(
                "Instructions for the model",
                "Naming, error handling, async style, anything the SDK should follow. Secrets are redacted before sending.",
                textarea("instructions", &self.instructions, window, cx),
                cx,
            ));
        self.page(
            "Instructions",
            "Optional guidance sent with every request in Generate and Port.",
            None,
            body,
            cx,
        )
    }

    fn run(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.omarchy().clone();
        let (label, status) = self.status();
        let right = self.pill(label, status, cx).into_any_element();
        let Some(session) = &self.session else {
            let body = self.empty(
                "Nothing running yet",
                "Start Generate, Maintain or Port from the Project screen and progress shows up here.",
                cx,
            );
            return self.page("Run", "Live progress for every language.", None, body, cx);
        };
        let mut grid = div().flex().flex_wrap().gap_4();
        for (scope, lane) in &session.lanes {
            let title = if scope.is_empty() { "Run" } else { scope };
            let mut log = div()
                .id(SharedString::from(format!("log-{title}")))
                .flex()
                .flex_col()
                .gap_0p5()
                .h(rems(17.))
                .p_3()
                .rounded_lg()
                .overflow_y_scroll()
                .bg(t.inset)
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
                self.card(cx)
                    .flex_1()
                    .min_w(rems(24.))
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(rems(1.))
                                    .font_weight(FontWeight::BOLD)
                                    .child(title.to_string()),
                            )
                            .child(
                                div()
                                    .text_size(rems(0.6875))
                                    .text_color(t.secondary)
                                    .child(format!("{} lines", lane.lines.len())),
                            ),
                    )
                    .child(div().text_size(rems(0.8125)).text_color(t.accent).child(
                        if lane.stage.is_empty() {
                            "starting…".to_string()
                        } else {
                            lane.stage.clone()
                        },
                    ))
                    .child(log),
            );
        }
        self.page(
            "Run",
            "Live progress for every language.",
            Some(right),
            grid,
            cx,
        )
    }

    fn review(&self, cx: &mut Context<Self>, window: &Window) -> impl IntoElement {
        let t = cx.omarchy().clone();
        let Some(req) = self.session.as_ref().and_then(|s| s.pending.as_ref()) else {
            let body = self.empty(
                "No diff waiting",
                "When a patch or a set of files passes the sandbox, it appears here for your approval.",
                cx,
            );
            return self.page("Diff review", "Approve what gets written.", None, body, cx);
        };
        let mut diff = div()
            .id("diff")
            .flex()
            .flex_col()
            .max_h(rems(30.))
            .rounded_lg()
            .overflow_y_scroll()
            .bg(t.inset)
            .py_2()
            .font_family(t.mono_font.clone())
            .text_size(rems(0.6875));
        for line in req.diff.lines() {
            let (color, tint) = match line.chars().next() {
                Some('+') => (t.success, 0.10),
                Some('-') => (t.danger, 0.10),
                Some('@') => (t.accent, 0.06),
                _ => (t.foreground, 0.0),
            };
            diff = diff.child(
                div()
                    .px_4()
                    .bg(color.opacity(tint))
                    .text_color(color)
                    .child(line.to_string()),
            );
        }
        let action = |id: &'static str, text: &'static str| {
            div()
                .id(id)
                .px_5()
                .py_2()
                .rounded_lg()
                .cursor_pointer()
                .text_size(rems(0.8125))
                .font_weight(FontWeight::BOLD)
                .child(text)
        };
        let body = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(self.card(cx).child(diff))
            .child(
                self.card(cx)
                    .child(self.field(
                        "Not quite right?",
                        "Describe what to change and the model tries another round.",
                        input("feedback", &self.feedback, window, cx),
                        cx,
                    ))
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(
                                action("approve", "Approve")
                                    .bg(t.success)
                                    .text_color(t.on_accent)
                                    .hover(|h| h.opacity(0.88))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.answer(Verdict::Approve, cx)
                                    })),
                            )
                            .child(
                                action("feedback-send", "Send feedback")
                                    .bg(t.foreground.opacity(0.10))
                                    .hover(|h| h.bg(t.foreground.opacity(0.16)))
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
                                action("reject", "Reject")
                                    .text_color(t.danger)
                                    .hover(|h| h.bg(t.danger.opacity(0.12)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.answer(Verdict::Reject, cx)
                                    })),
                            ),
                    ),
            );
        let right = self
            .pill("Tests pass", Status::Success, cx)
            .into_any_element();
        self.page(
            "Diff review",
            "Verified in the sandbox. Nothing is written until you approve.",
            Some(right),
            body,
            cx,
        )
    }

    fn output(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.omarchy().clone();
        let Some(results) = self.session.as_ref().and_then(|s| s.finished.as_ref()) else {
            let body = self.empty(
                "No finished run",
                "Results for each language show up here when a run completes.",
                cx,
            );
            return self.page("Output", "What came out of the last run.", None, body, cx);
        };
        let mut col = div().flex().flex_col().gap_4();
        for (lang, outcome) in results {
            let name = if lang.is_empty() { "Result" } else { lang };
            let (label, status) = if outcome.ok {
                ("Verified", Status::Success)
            } else {
                ("Needs attention", Status::Error)
            };
            col = col.child(
                self.card(cx)
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(rems(1.))
                                    .font_weight(FontWeight::BOLD)
                                    .child(name.to_string()),
                            )
                            .child(self.pill(label, status, cx)),
                    )
                    .child(
                        div()
                            .p_3()
                            .rounded_lg()
                            .bg(t.inset)
                            .font_family(t.mono_font.clone())
                            .text_size(rems(0.75))
                            .child(outcome.text.clone()),
                    ),
            );
        }
        self.page("Output", "What came out of the last run.", None, col, cx)
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
                    .px_10()
                    .py_8()
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

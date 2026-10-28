//! An explicitly authorized cloud preview, followed by a separate local move.
//! State lives on Gyotaku rather than Page so leaving never drops a live job.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{
    AnyElement, ClickEvent, Context, CursorStyle, ListAlignment, ListState, PathPromptOptions,
    SharedString, Window, div, list, prelude::*, px,
};
use gyotaku_core::categorize::{self, Category, Evidence, Preview};

use super::{Gyotaku, Page, settings::Key, thousands};
use crate::theme::Theme;

pub(super) struct Categorize {
    folder: Option<PathBuf>,
    images: bool,
    preview: Option<Arc<Preview>>,
    skipped: HashMap<usize, Category>,
    rows: ListState,
    job: Option<Job>,
    picking: bool,
    confirmation: Option<Confirmation>,
    message: Option<String>,
    cursor: usize,
}

impl Default for Categorize {
    fn default() -> Self {
        Self {
            folder: None,
            images: false,
            preview: None,
            skipped: HashMap::new(),
            rows: ListState::new(0, ListAlignment::Top, px(300.)),
            job: None,
            picking: false,
            confirmation: None,
            message: None,
            cursor: 0,
        }
    }
}

#[derive(Clone, Copy)]
enum Confirmation {
    Organize,
    Undo,
}

#[derive(Clone, Copy)]
enum Operation {
    Analyze,
    Organize,
    Undo,
}

impl Operation {
    fn running(self) -> &'static str {
        match self {
            Self::Analyze => "Analyzing",
            Self::Organize => "Organizing",
            Self::Undo => "Undoing organization",
        }
    }
}

struct Job {
    operation: Operation,
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<Progress>>,
}

#[derive(Default)]
struct Progress {
    done: usize,
    total: usize,
    result: Option<Result<Outcome, String>>,
}

enum Outcome {
    Preview(Preview),
    Summary(String),
}

enum Work {
    Analyze(PathBuf, Evidence),
    Organize(Arc<Preview>),
    Undo(PathBuf),
}

impl Gyotaku {
    pub(super) fn open_categorize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_recording(cx);
        self.confirming = false;
        self.page = Page::Categorize;
        window.focus(&self.panel_focus, cx);
        cx.notify();
    }

    pub(super) fn categorize_label(&self) -> SharedString {
        if let Some(job) = &self.categorize.job {
            format!("Categorize: {}", job.operation.running().to_lowercase()).into()
        } else if self.categorize.preview.is_some() {
            "Categorize: preview ready".into()
        } else {
            "Categorize".into()
        }
    }

    fn categorize_pick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.categorize.job.is_some() || self.categorize.picking {
            return;
        }
        self.categorize.picking = true;
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Categorize this folder".into()),
        });
        let step_aside = self.floating && crate::is_resident(cx);
        if step_aside {
            window.remove_window();
        }
        cx.spawn(async move |this, cx| {
            let result = picked.await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.categorize;
                state.picking = false;
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(folder) = paths.into_iter().next() {
                            state.folder = Some(folder);
                            state.preview = None;
                            state.skipped.clear();
                            state.rows.reset(0);
                            state.confirmation = None;
                            state.message = None;
                        }
                    }
                    Ok(Ok(None)) => {}
                    _ => {
                        state.message = Some("Could not open the folder picker. Try again.".into());
                    }
                }
                cx.notify();
            });
            if step_aside {
                cx.update(crate::summon);
            }
        })
        .detach();
        cx.notify();
    }

    fn categorize_start(&mut self, operation: Operation, cx: &mut Context<Self>) {
        let state = &mut self.categorize;
        if state.job.is_some() || state.picking {
            return;
        }
        let Some(folder) = state.folder.clone() else {
            return;
        };
        let work = match operation {
            Operation::Analyze => Work::Analyze(
                folder,
                if state.images {
                    Evidence::Images
                } else {
                    Evidence::NamesOnly
                },
            ),
            Operation::Organize => {
                let Some(preview) = state.preview.clone() else {
                    return;
                };
                Work::Organize(preview)
            }
            Operation::Undo => Work::Undo(folder),
        };
        state.confirmation = None;
        state.message = None;
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(Mutex::new(Progress::default()));
        let worker_progress = progress.clone();
        let worker_cancel = cancel.clone();
        let spawned = std::thread::Builder::new()
            .name("categorize".into())
            .spawn(move || {
                let result = match work {
                    Work::Analyze(folder, evidence) => categorize::api_key()
                        .and_then(|key| {
                            categorize::preview(
                                &folder,
                                evidence,
                                &key,
                                &worker_cancel,
                                |done, total| {
                                    let mut progress =
                                        worker_progress.lock().unwrap_or_else(|e| e.into_inner());
                                    progress.done = done;
                                    progress.total = total;
                                },
                            )
                        })
                        .map(Outcome::Preview),
                    Work::Organize(preview) => categorize::organize(&preview).map(Outcome::Summary),
                    Work::Undo(folder) => categorize::undo(&folder).map(Outcome::Summary),
                }
                .map_err(|error| error.to_string());
                worker_progress
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .result = Some(result);
            });
        if let Err(error) = spawned {
            state.message = Some(format!("Could not start the background job: {error}"));
            cx.notify();
            return;
        }
        // A preview is consumed by a move attempt, even if that attempt only
        // partially succeeds. The persisted journal is the recovery path.
        state.preview = None;
        state.skipped.clear();
        state.rows.reset(0);
        state.job = Some(Job {
            operation,
            cancel,
            progress,
        });
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let running = this.update(cx, |this, cx| {
                    let state = &mut this.categorize;
                    let Some(job) = &state.job else { return false };
                    let result = job
                        .progress
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .result
                        .take();
                    if let Some(result) = result {
                        let cancelled = matches!(job.operation, Operation::Analyze)
                            && job.cancel.load(Ordering::Relaxed);
                        state.job = None;
                        if cancelled {
                            state.message = Some("Analysis cancelled. No files were moved.".into());
                        } else {
                            match result {
                                Ok(Outcome::Preview(preview)) => {
                                    state.rows.reset(preview.entries.len());
                                    state.preview = Some(Arc::new(preview));
                                    state.cursor = 4;
                                }
                                Ok(Outcome::Summary(summary)) => state.message = Some(summary),
                                Err(error) => {
                                    state.message = Some(error);
                                }
                            }
                        }
                        cx.notify();
                        false
                    } else {
                        if matches!(this.page, Page::Categorize) {
                            cx.notify();
                        }
                        true
                    }
                });
                if !matches!(running, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn categorize_activate(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(operation) = self.categorize.confirmation {
            if at != 6 && at != 7 {
                return;
            }
            self.categorize.confirmation = None;
            self.categorize.cursor = 0;
            if at == 7 {
                self.categorize_start(
                    match operation {
                        Confirmation::Organize => Operation::Organize,
                        Confirmation::Undo => Operation::Undo,
                    },
                    cx,
                );
            }
            cx.notify();
            return;
        }
        if self.categorize.picking {
            return;
        }
        if let Some(job) = &self.categorize.job {
            if at == 3 && matches!(job.operation, Operation::Analyze) {
                job.cancel.store(true, Ordering::Relaxed);
                cx.notify();
            }
            return;
        }
        match at {
            0 => self.categorize_pick(window, cx),
            1 | 2 => {
                self.categorize.images = at == 2;
                cx.notify();
            }
            3 => self.categorize_start(Operation::Analyze, cx),
            4 if self.categorize.preview.as_ref().is_some_and(|p| {
                p.entries
                    .iter()
                    .any(|e| e.category.is_some() && e.error.is_none())
            }) =>
            {
                self.categorize.confirmation = Some(Confirmation::Organize);
                self.categorize.cursor = 6;
                cx.notify();
            }
            5 if self.categorize.folder.is_some() => {
                self.categorize.confirmation = Some(Confirmation::Undo);
                self.categorize.cursor = 6;
                cx.notify();
            }
            6.. => self.categorize_toggle_file(at - 6, cx),
            _ => {}
        }
    }

    pub(super) fn categorize_key(&mut self, key: Key, window: &mut Window, cx: &mut Context<Self>) {
        let (first, last) = if self.categorize.confirmation.is_some() {
            (6, 7)
        } else {
            (
                0,
                5 + self
                    .categorize
                    .preview
                    .as_ref()
                    .map_or(0, |p| p.entries.len()),
            )
        };
        match key {
            Key::Up | Key::Left => {
                self.categorize.cursor = self.categorize.cursor.saturating_sub(1).max(first)
            }
            Key::Down | Key::Right => {
                self.categorize.cursor = (self.categorize.cursor + 1).min(last)
            }
            Key::Enter | Key::Space => self.categorize_activate(self.categorize.cursor, window, cx),
            Key::Remove => {}
        }
        if self.categorize.confirmation.is_none() && self.categorize.cursor >= 6 {
            self.categorize
                .rows
                .scroll_to_reveal_item(self.categorize.cursor - 6);
        }
        cx.notify();
    }

    pub(super) fn categorize_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.categorize.confirmation.take().is_some() {
            self.categorize.cursor = 0;
            cx.notify();
        } else {
            self.leave_panel(window, cx);
        }
    }

    fn categorize_control(
        &self,
        at: usize,
        label: impl Into<SharedString>,
        enabled: bool,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label: SharedString = label.into();
        div()
            .id(("categorize-control", at))
            .px_3()
            .py_2()
            .rounded(px(8.))
            .text_sm()
            .border_1()
            .border_color(if self.categorize.cursor == at {
                theme.muted
            } else {
                theme.hairline
            })
            .text_color(if enabled { theme.text } else { theme.muted })
            .when(enabled, |el| {
                el.cursor(CursorStyle::PointingHand)
                    .hover(move |el| el.bg(theme.hover_wash))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.categorize_activate(at, window, cx);
                    }))
            })
            .child(label)
            .into_any_element()
    }

    fn categorize_toggle_file(&mut self, index: usize, cx: &mut Context<Self>) {
        let state = &mut self.categorize;
        if state.job.is_some() || state.confirmation.is_some() || state.picking {
            return;
        }
        let Some(preview) = state.preview.as_mut().and_then(Arc::get_mut) else {
            return;
        };
        let Some(entry) = preview.entries.get_mut(index) else {
            return;
        };
        if entry.error.is_some() {
            return;
        }
        if let Some(category) = state.skipped.remove(&index) {
            entry.category = Some(category);
        } else if let Some(category) = entry.category.take() {
            state.skipped.insert(index, category);
        }
        state.cursor = index + 6;
        cx.notify();
    }

    fn render_categorize_rows(
        &mut self,
        i: usize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.global::<Theme>();
        let Some(preview) = &self.categorize.preview else {
            return div().into_any_element();
        };
        let Some(entry) = preview.entries.get(i) else {
            return div().into_any_element();
        };
        let name = entry
            .source
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let skipped = self.categorize.skipped.contains_key(&i);
        let can_toggle = (entry.category.is_some() || skipped)
            && entry.error.is_none()
            && self.categorize.confirmation.is_none();
        let destination = match (entry.category, &entry.error) {
            (_, Some(error)) => format!("Unchanged: {error}"),
            (None, None) if skipped => "Unchanged: skipped by you".into(),
            (Some(category), None) => {
                format!("{}: {} / {name}", category.label(), category.folder_name())
            }
            (None, None) => "Unchanged: no confident category".into(),
        };
        div()
            .px_6()
            .py_2()
            .border_b_1()
            .border_color(theme.hairline)
            .flex()
            .items_center()
            .gap_3()
            .when(
                self.categorize.confirmation.is_none() && self.categorize.cursor == i + 6,
                |el| el.bg(theme.hover_wash),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .child(div().text_sm().text_ellipsis().child(name))
                    .child(div().text_xs().text_color(theme.muted).child(destination)),
            )
            .when(can_toggle, |el| {
                el.child(
                    div()
                        .id(("categorize-skip", i))
                        .px_3()
                        .py_2()
                        .rounded(px(8.))
                        .text_sm()
                        .cursor(CursorStyle::PointingHand)
                        .hover(move |el| el.bg(theme.hover_wash))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.categorize_toggle_file(i, cx)
                        }))
                        .child(if skipped { "Include" } else { "Skip" }),
                )
            })
            .into_any_element()
    }

    pub(super) fn render_categorize(&mut self, theme: Theme, cx: &mut Context<Self>) -> AnyElement {
        let state = &self.categorize;
        let idle = state.job.is_none() && !state.picking && state.confirmation.is_none();
        let has_folder = state.folder.is_some();
        let folder = state
            .folder
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "No folder selected".into());
        let moves = state.preview.as_ref().map_or(0, |p| {
            p.entries
                .iter()
                .filter(|e| e.category.is_some() && e.error.is_none())
                .count()
        });
        let total = state.preview.as_ref().map_or(0, |p| p.entries.len());
        let rows = state.rows.clone();
        let mut settings = div().flex_none().px_6().py_3().flex().flex_col().gap_2()
            .child(div().text_sm().child("Categorize immediate files into subfolders. Existing subfolders are not scanned."))
            .child(div().flex().items_center().gap_3()
                .child(self.categorize_control(0, if state.picking { "Choosing folder..." } else { "Choose folder..." }, idle, theme, cx))
                .child(div().flex_1().min_w(px(0.)).text_sm().text_ellipsis().child(folder.clone())))
            .child(div().flex().gap_2()
                .child(self.categorize_control(1, if state.images { "Names only" } else { "Names only (selected)" }, idle, theme, cx))
                .child(self.categorize_control(2, if state.images { "Include images (selected)" } else { "Include images" }, idle, theme, cx)))
            .child(div().text_xs().text_color(theme.muted).child(if state.images {
                "Analyze authorizes uploading filenames and supported image previews to Jev via Typesafe. Other file contents and full paths are not sent."
            } else {
                "Analyze authorizes sending filenames to Jev via Typesafe. File contents, images and full paths stay local."
            }))
            .child(div().text_xs().text_color(theme.muted).child("Uses TYPESAFE_API_KEY from your environment or .env. Your key is never saved in app settings."));
        if let Some(operation) = state.confirmation {
            let question = match operation {
                Confirmation::Organize => format!(
                    "Move {} files into the previewed subfolders of {folder}? This creates real folders and moves files. Errors remain untouched.",
                    thousands(moves)
                ),
                Confirmation::Undo => format!(
                    "Restore all files still recorded as organized in {folder}? Undo uses this folder's persisted journal, including after restarting the app. Conflicts and changed files stay untouched."
                ),
            };
            settings = settings.child(
                div()
                    .p_3()
                    .rounded(px(8.))
                    .bg(theme.keycap)
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_sm().child(question))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(self.categorize_control(6, "Cancel", true, theme, cx))
                            .child(self.categorize_control(
                                7,
                                match operation {
                                    Confirmation::Undo => "Confirm undo",
                                    Confirmation::Organize => "Confirm organize",
                                },
                                true,
                                theme,
                                cx,
                            )),
                    ),
            );
        } else if let Some(job) = &state.job {
            let progress = job.progress.lock().unwrap_or_else(|e| e.into_inner());
            let status = if matches!(job.operation, Operation::Analyze) {
                if job.cancel.load(Ordering::Relaxed) {
                    "Cancelling after the current request...".into()
                } else if progress.total == 0 {
                    "Analyzing: checking API key and reading folder...".into()
                } else {
                    format!(
                        "Analyzing: {} of {} files",
                        thousands(progress.done),
                        thousands(progress.total)
                    )
                }
            } else {
                format!(
                    "{}... Let this finish safely; undo is saved on disk.",
                    job.operation.running()
                )
            };
            settings = settings.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_1().text_sm().child(status))
                    .when(matches!(job.operation, Operation::Analyze), |el| {
                        el.child(self.categorize_control(
                            3,
                            "Cancel analysis",
                            !job.cancel.load(Ordering::Relaxed),
                            theme,
                            cx,
                        ))
                    }),
            );
        } else {
            settings = settings.child(
                div()
                    .flex()
                    .gap_2()
                    .child(self.categorize_control(3, "Analyze", idle && has_folder, theme, cx))
                    .child(self.categorize_control(4, "Organize...", idle && moves > 0, theme, cx))
                    .child(self.categorize_control(
                        5,
                        "Undo organization...",
                        idle && has_folder,
                        theme,
                        cx,
                    )),
            );
        }
        if let Some(message) = &state.message {
            settings = settings.child(
                div()
                    .text_sm()
                    .text_color(theme.text)
                    .child(message.clone()),
            );
        }
        let has_preview = state.preview.is_some();
        div().flex_1().min_h(px(0.)).flex().flex_col()
            .child(div().flex_none().h(px(super::HEADER)).px_6().flex().items_center().justify_between()
                .border_b_1().border_color(theme.hairline)
                .child(div().text_size(px(21.)).child("Categorize"))
                .child(div().id("categorize-back").px_3().py_2().rounded(px(8.)).text_sm()
                    .cursor(CursorStyle::PointingHand).hover(move |el| el.bg(theme.hover_wash))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.categorize_back(window, cx)))
                    .child("Back to search")))
            .child(settings)
            .child(div().flex_none().px_6().py_2().text_sm().child(if has_preview {
                format!("Preview: {} files, {} to move, {} unchanged. Skip any file to leave it in place.", thousands(total), thousands(moves), thousands(total - moves))
            } else { "Preview".into() }))
            .child(if has_preview {
                list(rows, cx.processor(Self::render_categorize_rows)).flex_1().min_h(px(0.)).into_any_element()
            } else {
                div().flex_1().min_h(px(0.)).px_6().py_3().text_sm().text_color(theme.muted)
                    .child("Choose a folder and Analyze to review destinations before anything moves. To undo an earlier organization, choose that folder and use Undo without analyzing.").into_any_element()
            })
            .child(div().flex_none().px_6().py_2().text_xs().text_color(theme.muted)
                .child("Use Up / Down to select, Enter to activate, Esc to return. Jobs and previews stay available when you leave this screen."))
            .into_any_element()
    }
}

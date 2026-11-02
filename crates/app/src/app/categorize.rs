//! An explicitly authorized cloud preview, followed by a separate local move.
//! State lives on Gyotaku rather than Page so leaving never drops a live job.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{
    AnyElement, ClickEvent, Context, CursorStyle, FontWeight, ListAlignment, ListState,
    PathPromptOptions, SharedString, Window, div, list, prelude::*, px, relative,
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
        let selected = matches!(at, 1 | 2) && self.categorize.images == (at == 2);
        let primary = enabled
            && (at == 4
                || at == 7
                || (at == 0 && self.categorize.folder.is_none())
                || (at == 3 && self.categorize.job.is_none() && self.categorize.preview.is_none()));
        let background = if primary {
            theme.text
        } else if selected {
            theme.panel
        } else {
            theme.keycap
        };
        div()
            .id(("categorize-control", at))
            .h(px(36.))
            .px_4()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(18.))
            .text_size(px(14.))
            .font_weight(FontWeight::MEDIUM)
            .bg(background)
            .border_1()
            .border_color(if self.categorize.cursor == at || selected {
                theme.muted
            } else if primary {
                theme.text
            } else {
                theme.hairline
            })
            .text_color(if primary {
                theme.panel
            } else if enabled {
                theme.text
            } else {
                theme.muted
            })
            .when(enabled, |el| {
                el.cursor(CursorStyle::PointingHand)
                    .hover(move |el| {
                        if primary {
                            el.border_color(theme.muted)
                        } else {
                            el.bg(theme.hover_wash)
                        }
                    })
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
        window: &mut Window,
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
        let compact = window.viewport_size().width < px(760.);
        let narrow = window.viewport_size().width < px(560.);
        let destination = match (entry.category, &entry.error) {
            (_, Some(_)) => "Stays here: needs attention",
            (None, None) if skipped => "Stays here: skipped by you",
            (Some(category), None) => category.folder_name(),
            (None, None) => "Stays here: no confident category",
        };
        let extension = entry
            .source
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("file")
            .to_ascii_uppercase();
        // The source and destination share the available width; the trailing
        // 84px action column stays aligned with the table heading.
        div()
            .w_full()
            .px_6()
            .py_3()
            .border_b_1()
            .border_color(theme.hairline)
            .when(
                self.categorize.confirmation.is_none() && self.categorize.cursor == i + 6,
                |el| el.bg(theme.hover_wash),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .items_center()
                            .gap_3()
                            .when(!narrow, |el| {
                                el.child(
                                    div()
                                        .w(px(46.))
                                        .flex_none()
                                        .text_size(px(11.))
                                        .text_color(theme.muted)
                                        .text_ellipsis()
                                        .child(extension),
                                )
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .child(div().text_size(px(14.)).text_ellipsis().child(name))
                                    .when(compact, |el| {
                                        el.child(
                                            div()
                                                .mt_1()
                                                .text_size(px(12.))
                                                .text_color(theme.muted)
                                                .text_ellipsis()
                                                .child(destination),
                                        )
                                    }),
                            ),
                    )
                    .when(!compact, |el| {
                        el.child(
                            div()
                                .w(relative(0.34))
                                .min_w(px(0.))
                                .text_size(px(14.))
                                .text_color(if entry.category.is_some() {
                                    theme.text
                                } else {
                                    theme.muted
                                })
                                .text_ellipsis()
                                .child(destination),
                        )
                    })
                    .child(div().w(px(84.)).flex_none().flex().justify_end().when(
                        can_toggle,
                        |el| {
                            el.child(
                                div()
                                    .id(("categorize-skip", i))
                                    .h(px(36.))
                                    .px_3()
                                    .rounded(px(18.))
                                    .flex()
                                    .items_center()
                                    .text_size(px(13.))
                                    .cursor(CursorStyle::PointingHand)
                                    .hover(move |el| el.bg(theme.keycap))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.categorize_toggle_file(i, cx)
                                    }))
                                    .child(if skipped { "Include" } else { "Skip" }),
                            )
                        },
                    )),
            )
            .when_some(entry.error.as_ref(), |el, error| {
                el.child(
                    div()
                        .mt_2()
                        .text_size(px(12.))
                        .text_color(theme.muted)
                        .child(error.clone()),
                )
            })
            .into_any_element()
    }

    pub(super) fn render_categorize(
        &mut self,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = &self.categorize;
        let compact = window.viewport_size().width < px(760.);
        let narrow = window.viewport_size().width < px(560.);
        let idle = state.job.is_none() && !state.picking && state.confirmation.is_none();
        let has_folder = state.folder.is_some();
        let has_preview = state.preview.is_some();
        let folder_name = state
            .folder
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| {
                if has_folder {
                    "Selected folder"
                } else {
                    "Choose a folder to organize"
                }
                .into()
            });
        let folder = state
            .folder
            .as_ref()
            .map(|p| gyotaku_core::tidy(p))
            .unwrap_or_else(|| "Downloads, documents, or any folder on this computer".into());
        let total = state.preview.as_ref().map_or(0, |p| p.entries.len());
        let mut counts = [0usize; Category::ALL.len()];
        if let Some(preview) = &state.preview {
            for entry in &preview.entries {
                if let Some(category) = entry.category.filter(|_| entry.error.is_none())
                    && let Some(index) = Category::ALL.iter().position(|c| *c == category)
                {
                    counts[index] += 1;
                }
            }
        }
        let moves: usize = counts.iter().sum();
        let folders = counts.iter().filter(|n| **n > 0).count();
        let rows = state.rows.clone();

        let source = div()
            .flex_none()
            .mx_6()
            .mt_4()
            .p_4()
            .rounded(px(12.))
            .bg(theme.keycap)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .text_size(px(18.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_ellipsis()
                                    .child(folder_name),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_size(px(12.))
                                    .text_color(theme.muted)
                                    .text_ellipsis()
                                    .child(folder),
                            ),
                    )
                    .child(self.categorize_control(
                        0,
                        if state.picking {
                            "Choosing..."
                        } else if has_folder {
                            "Change folder"
                        } else {
                            "Choose folder"
                        },
                        idle,
                        theme,
                        cx,
                    )),
            );

        let setup = div().flex_none().px_6().py_4().flex().flex_col().gap_3()
            .child(div().flex().flex_wrap().items_center().gap_3()
                .child(div().text_size(px(14.)).font_weight(FontWeight::MEDIUM).child("Send to Jev"))
                .child(div().flex().gap_1()
                    .child(self.categorize_control(1, "Names only", idle, theme, cx))
                    .child(self.categorize_control(2, "Include images", idle, theme, cx)))
                .when(!has_preview && state.job.is_none() && state.confirmation.is_none(), |el| el.child(
                    self.categorize_control(3, "Analyze folder", idle && has_folder, theme, cx))))
            .child(div().text_size(px(13.)).text_color(theme.muted).child(if state.images {
                "Analyze uploads filenames and supported image previews to TypeSafe. Image analysis is experimental."
            } else {
                "Analyze sends filenames to TypeSafe. File contents and images stay on this computer."
            }))
            .child(div().text_size(px(12.)).text_color(theme.muted)
                .child(if narrow { "Subfolders stay untouched. Full paths stay local." } else {
                    "Only files directly in this folder. Existing subfolders are left alone. Full paths are never sent."
                }));

        let mut activity = div().flex_none().px_6().flex().flex_col().gap_2();
        if let Some(job) = &state.job {
            let progress = job.progress.lock().unwrap_or_else(|e| e.into_inner());
            let analyzing = matches!(job.operation, Operation::Analyze);
            let cancelling = job.cancel.load(Ordering::Relaxed);
            let status = if analyzing {
                if cancelling {
                    "Cancelling after the current request".into()
                } else if progress.total == 0 {
                    "Reading folder and preparing analysis".into()
                } else {
                    format!(
                        "Analyzing {} of {} files",
                        thousands(progress.done),
                        thousands(progress.total)
                    )
                }
            } else {
                format!("{}; recovery is saved on disk", job.operation.running())
            };
            activity = activity
                .pb_4()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(div().flex_1().text_size(px(14.)).child(status))
                        .when(analyzing, |el| {
                            el.child(self.categorize_control(
                                3,
                                "Cancel analysis",
                                !cancelling,
                                theme,
                                cx,
                            ))
                        }),
                )
                .when(analyzing && progress.total > 0, |el| {
                    el.child(
                        div()
                            .w_full()
                            .h(px(4.))
                            .rounded(px(2.))
                            .bg(theme.track)
                            .child(div().h_full().rounded(px(2.)).bg(theme.text).w(relative(
                                (progress.done as f32 / progress.total.max(1) as f32).clamp(0., 1.),
                            ))),
                    )
                })
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.muted)
                        .child("You can return to search. This job will keep running."),
                );
        }
        if let Some(message) = &state.message {
            activity = activity.child(
                div()
                    .id("categorize-result")
                    .max_h(px(96.))
                    .overflow_y_scroll()
                    .mb_4()
                    .p_3()
                    .rounded(px(8.))
                    .bg(theme.keycap)
                    .text_size(px(13.))
                    .child(message.clone()),
            );
        }

        let mut workspace =
            div().flex_1().min_h(px(0.)).flex().flex_col().child(
                div()
                    .flex_none()
                    .px_6()
                    .py_3()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_weight(FontWeight::MEDIUM)
                            .child("Destination preview"),
                    )
                    .child(div().text_size(px(13.)).text_color(theme.muted).child(
                        if has_preview {
                            format!(
                                "{} files reviewed / {} staying here",
                                thousands(total),
                                thousands(total - moves)
                            )
                        } else {
                            "Nothing moves during analysis".into()
                        },
                    )),
            );
        if has_preview && folders > 0 && !narrow {
            workspace = workspace.child(
                div()
                    .flex_none()
                    .px_6()
                    .pb_3()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .children(
                        Category::ALL
                            .iter()
                            .zip(counts)
                            .filter(|(_, count)| *count > 0)
                            .map(|(category, count)| {
                                div()
                                    .px_3()
                                    .py_1()
                                    .rounded(px(6.))
                                    .bg(theme.keycap)
                                    .text_size(px(12.))
                                    .child(format!(
                                        "{}  {}",
                                        category.folder_name(),
                                        thousands(count)
                                    ))
                            }),
                    ),
            );
        }
        if has_preview && total > 0 {
            workspace = workspace
                .child(
                    div()
                        .flex_none()
                        .px_6()
                        .py_2()
                        .flex()
                        .items_center()
                        .gap_4()
                        .text_size(px(12.))
                        .text_color(theme.muted)
                        .child(div().flex_1().child(if compact {
                            "File and destination folder"
                        } else {
                            "File"
                        }))
                        .when(!compact, |el| {
                            el.child(div().w(relative(0.34)).child("Destination folder"))
                        })
                        .child(div().w(px(84.)).flex_none().text_right().child("Review")),
                )
                .child(
                    list(rows, cx.processor(Self::render_categorize_rows))
                        .w_full()
                        .flex_1()
                        .min_h(px(0.)),
                );
        } else {
            let (title, detail) = if state.job.is_some() {
                (
                    "Your preview is being prepared",
                    "Proposed destinations will appear here when analysis finishes.",
                )
            } else if has_preview {
                (
                    "No eligible files in this folder",
                    "Subfolders, hidden files, incomplete downloads, and credential-like filenames are not analyzed.",
                )
            } else if has_folder {
                (
                    "Ready to find a place for your files",
                    "Analyze this folder to review each file beside its proposed destination. You choose what moves.",
                )
            } else {
                (
                    "A place for every file",
                    "Choose a folder above. Jev suggests destinations; you review the plan before creating folders or moving files.",
                )
            };
            workspace = workspace.child(div().px_6().py_5().flex().flex_col().gap_2()
                .child(div().text_size(px(18.)).child(title))
                .child(div().max_w(px(580.)).text_size(px(14.)).text_color(theme.muted).child(detail))
                .when(!has_folder, |el| el.child(div().mt_3().text_size(px(12.))
                    .text_color(theme.muted).child("Uses TYPESAFE_API_KEY from your environment or .env. Your key is never saved in settings."))));
        }

        let mut footer = div()
            .flex_none()
            .px_6()
            .py_4()
            .bg(theme.keycap)
            .flex()
            .flex_col()
            .gap_3();
        if let Some(confirmation) = state.confirmation {
            let (title, detail, confirm) = match confirmation {
                Confirmation::Organize => (
                    format!(
                        "Move {} files into {} category folders?",
                        thousands(moves),
                        thousands(folders)
                    ),
                    "Folders are created inside the selected folder. Existing files are never overwritten. You can undo these moves.",
                    "Create folders and move",
                ),
                Confirmation::Undo => (
                    "Restore all recorded moves in this folder?".into(),
                    "Includes earlier organization runs. Changed files and occupied original locations stay untouched.",
                    "Restore files",
                ),
            };
            footer = footer
                .child(
                    div()
                        .text_size(px(16.))
                        .font_weight(FontWeight::MEDIUM)
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(theme.muted)
                        .child(detail),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .justify_end()
                        .gap_2()
                        .child(self.categorize_control(
                            6,
                            match confirmation {
                                Confirmation::Organize => "Keep reviewing",
                                Confirmation::Undo => "Cancel",
                            },
                            true,
                            theme,
                            cx,
                        ))
                        .child(self.categorize_control(7, confirm, true, theme, cx)),
                );
        } else {
            footer = footer.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(if has_preview {
                                        format!(
                                            "{} files to move into {} folders",
                                            thousands(moves),
                                            thousands(folders)
                                        )
                                    } else {
                                        "Review first. Move only when ready.".into()
                                    }),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme.muted)
                                    .child("No overwrites. A local journal keeps moves undoable."),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .when(compact, |el| el.w_full())
                            .justify_end()
                            .gap_2()
                            .child(self.categorize_control(
                                5,
                                "Undo organization",
                                idle && has_folder,
                                theme,
                                cx,
                            ))
                            .when(has_preview, |el| {
                                el.child(self.categorize_control(
                                    3,
                                    "Analyze again",
                                    idle && has_folder,
                                    theme,
                                    cx,
                                ))
                                .child(self.categorize_control(
                                    4,
                                    "Review moves",
                                    idle && moves > 0,
                                    theme,
                                    cx,
                                ))
                            }),
                    ),
            );
        }

        div()
            .w_full()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px_6()
                    .py_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .text_color(theme.muted)
                                    .child("Nemo"),
                            )
                            .child(
                                div()
                                    .text_size(px(22.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("Categorize"),
                            ),
                    )
                    .child(
                        div()
                            .id("categorize-back")
                            .h(px(36.))
                            .px_3()
                            .flex()
                            .items_center()
                            .rounded(px(18.))
                            .text_size(px(13.))
                            .cursor(CursorStyle::PointingHand)
                            .hover(move |el| el.bg(theme.hover_wash))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.categorize_back(window, cx)
                            }))
                            .child("Back to search"),
                    ),
            )
            .child(source)
            .child(setup)
            .child(activity)
            .child(workspace)
            .child(footer)
            .into_any_element()
    }
}

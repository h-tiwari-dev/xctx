//! The main window's root view: Home or a space tab, under the tab bar and over the status bar, and the state
//! they share (the store, the repo index, the open spaces, the status banner).

use std::collections::{HashMap, HashSet};

use gpui::{
    Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render, SharedString,
    Styled, Subscription, Task, Window, div, prelude::*,
};
use xctx_core::{RepoIndex, Space, SpaceDraft, SpaceId, Store};
use xctx_layout::{FocusDirection, SplitAxis};
use xctx_term_view::{TerminalFont, TerminalTheme};
use xctx_ui::{PromptEvent, PromptInput, StatusBanner, StatusBar, TabBar, TabItem, Theme};

use crate::dialogs::Dialog;
use crate::keymap::{
    ActivateTab, ClosePane, CloseWindow, DismissDialog, FocusDown, FocusLeft, FocusNext, FocusPrev,
    FocusRight, FocusUp, LaunchDraft, ResizeDown, ResizeLeft, ResizeRight, ResizeUp, ShowHome,
    SplitDown, SplitRight, ToggleZoom, WORKSPACE_CONTEXT, home_key_hints, space_key_hints,
};
use crate::lifecycle::scan_index;
use crate::space_layout::layout_shape;
use crate::space_tab::SpaceTab;
use crate::spawn::ProgramSearch;

/// What the window shows below the tab bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum View {
    /// Home: the prompt, the draft, the living spaces and the indexed folders.
    Home,
    /// A space's panes.
    Space(SpaceId),
}

/// The root view of the main window.
pub(crate) struct Workspace {
    pub(crate) focus_handle: FocusHandle,
    /// The store; `None` while a background job (launch, ship, discard) has it, or when it could not be opened.
    pub(crate) store: Option<Store>,
    /// Why the store could not be opened.
    pub(crate) store_error: Option<String>,
    /// What a background job holding the store is doing, for the UI.
    pub(crate) busy: Option<SharedString>,
    /// The active spaces, as last read from the store.
    pub(crate) spaces: Vec<Space>,
    /// Spaces whose layout changed while the store was busy, saved once it is back.
    pub(crate) pending_persist: HashSet<SpaceId>,
    pub(crate) index: RepoIndex,
    pub(crate) scanning: bool,
    scan_generation: u64,
    /// A task typed while the index was being built, drafted once it is.
    pub(crate) pending_task: Option<String>,
    pub(crate) draft: Option<SpaceDraft>,
    pub(crate) task_prompt: Entity<PromptInput>,
    pub(crate) root_input: Entity<PromptInput>,
    pub(crate) repo_input: Entity<PromptInput>,
    pub(crate) view: View,
    /// Open space tabs, by space; a space's terminals start when it is first shown.
    pub(crate) tabs: HashMap<SpaceId, SpaceTab>,
    pub(crate) dialog: Option<Dialog>,
    pub(crate) dialog_focus: FocusHandle,
    /// The open dialog's subscription to its text input, if it has one.
    pub(crate) dialog_subscription: Option<Subscription>,
    banner: Option<StatusBanner>,
    banner_generation: u64,
    banner_task: Option<Task<()>>,
    pub(crate) persist_task: Option<Task<()>>,
    pub(crate) search: ProgramSearch,
    pub(crate) terminal_font: TerminalFont,
    pub(crate) terminal_theme: TerminalTheme,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for Workspace {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Workspace {
    /// The workspace over the store in the default data directory, with its active spaces as tabs.
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (store, store_error) = match Store::open_default() {
            Ok(store) => (Some(store), None),
            Err(e) => (None, Some(format!("could not open the xctx store: {e}"))),
        };
        let task_prompt = cx.new(|cx| {
            PromptInput::new(cx)
                .placeholder("What do you want to do? e.g. add rate limiting to checkout")
        });
        let root_input =
            cx.new(|cx| PromptInput::new(cx).placeholder("A folder with your repos, e.g. ~/code"));
        let repo_input =
            cx.new(|cx| PromptInput::new(cx).placeholder("Path to another repo to add"));
        let subscriptions = vec![
            cx.subscribe_in(
                &task_prompt,
                window,
                |this, _, event: &PromptEvent, window, cx| match event {
                    PromptEvent::Submit(task) => this.submit_task(task.clone(), cx),
                    PromptEvent::Cancel => this.cancel_draft(window, cx),
                },
            ),
            cx.subscribe_in(
                &root_input,
                window,
                |this, _, event: &PromptEvent, _, cx| {
                    if let PromptEvent::Submit(path) = event {
                        this.add_root(path.clone(), cx);
                    }
                },
            ),
            cx.subscribe_in(
                &repo_input,
                window,
                |this, _, event: &PromptEvent, _, cx| {
                    if let PromptEvent::Submit(path) = event {
                        this.add_repo_path(path.clone(), cx);
                    }
                },
            ),
            // A divider drag saves its layout after a pause; save everything once more on quit so none is lost.
            cx.on_app_quit(|this, cx| {
                let ids: Vec<SpaceId> = this.tabs.keys().cloned().collect();
                for id in &ids {
                    this.persist(id, cx);
                }
                async {}
            }),
        ];
        let mut workspace = Workspace {
            focus_handle: cx.focus_handle(),
            store,
            store_error,
            busy: None,
            spaces: Vec::new(),
            pending_persist: HashSet::new(),
            index: RepoIndex::default(),
            scanning: false,
            scan_generation: 0,
            pending_task: None,
            draft: None,
            task_prompt,
            root_input,
            repo_input,
            view: View::Home,
            tabs: HashMap::new(),
            dialog: None,
            dialog_focus: cx.focus_handle(),
            dialog_subscription: None,
            banner: None,
            banner_generation: 0,
            banner_task: None,
            persist_task: None,
            search: ProgramSearch::from_env(),
            terminal_font: TerminalFont::default(),
            terminal_theme: TerminalTheme::default(),
            _subscriptions: subscriptions,
        };
        workspace.refresh_spaces();
        workspace.rescan(cx);
        let prompt_focus = workspace.task_prompt.focus_handle(cx);
        prompt_focus.focus(window, cx);
        workspace
    }

    /// Re-read the active spaces from the store.
    pub(crate) fn refresh_spaces(&mut self) {
        if let Some(store) = &self.store {
            self.spaces = store.active_spaces().cloned().collect();
        }
    }

    /// Show a message in the status bar; info messages clear themselves.
    pub(crate) fn set_banner(&mut self, banner: StatusBanner, cx: &mut Context<Self>) {
        self.banner_generation += 1;
        let generation = self.banner_generation;
        self.banner_task = banner.auto_clear_after().map(|delay| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(delay).await;
                let _ = this.update(cx, |this, cx| {
                    if this.banner_generation == generation {
                        this.banner = None;
                        cx.notify();
                    }
                });
            })
        });
        self.banner = Some(banner);
        cx.notify();
    }

    /// Show an info message that clears itself.
    pub(crate) fn info(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.set_banner(StatusBanner::info(message), cx);
    }

    /// Show an error that stays until dismissed.
    pub(crate) fn error(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.set_banner(StatusBanner::error(message), cx);
    }

    fn dismiss_banner(&mut self, cx: &mut Context<Self>) {
        self.banner = None;
        self.banner_task = None;
        cx.notify();
    }

    /// Take the store for a background job that says it is doing `what`. `None`, with an error shown, when another
    /// job has it or there is no store.
    pub(crate) fn take_store(&mut self, what: &str, cx: &mut Context<Self>) -> Option<Store> {
        if let Some(busy) = &self.busy {
            self.error(format!("wait until {busy} finishes"), cx);
            return None;
        }
        match self.store.take() {
            Some(store) => {
                self.busy = Some(what.to_owned().into());
                cx.notify();
                Some(store)
            }
            None => {
                let message = self
                    .store_error
                    .clone()
                    .unwrap_or_else(|| "the store is not available".to_owned());
                self.error(message, cx);
                None
            }
        }
    }

    /// Put back the store a background job took, and save layouts that changed meanwhile.
    pub(crate) fn return_store(&mut self, store: Store, cx: &mut Context<Self>) {
        self.store = Some(store);
        self.busy = None;
        self.refresh_spaces();
        for id in std::mem::take(&mut self.pending_persist) {
            self.persist(&id, cx);
        }
        cx.notify();
    }

    /// Index the store's folders again, in the background.
    pub(crate) fn rescan(&mut self, cx: &mut Context<Self>) {
        let Some(store) = &self.store else {
            return;
        };
        let roots = store.roots().to_vec();
        self.scan_generation += 1;
        let generation = self.scan_generation;
        self.scanning = true;
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let index = executor.spawn(async move { scan_index(roots) }).await;
            let _ = this.update(cx, |this, cx| {
                if this.scan_generation != generation {
                    return;
                }
                this.index = index;
                this.scanning = false;
                if let Some(task) = this.pending_task.take() {
                    this.submit_task(task, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Show Home.
    pub(crate) fn show_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.view = View::Home;
        self.close_idle_dialog();
        let focus = self.task_prompt.focus_handle(cx);
        focus.focus(window, cx);
        cx.notify();
    }

    /// Show the space's tab, starting its terminals if this is the first time.
    pub(crate) fn show_space(&mut self, id: &SpaceId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.spaces.iter().any(|s| &s.id == id) {
            self.error(format!("space {id} is not active"), cx);
            return;
        }
        self.open_space(id, window, cx);
        if !self.tabs.contains_key(id) {
            return;
        }
        if self.view != View::Space(id.clone()) {
            self.close_idle_dialog();
        }
        self.view = View::Space(id.clone());
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    /// Close the dialog unless its job is running; a running job's dialog stays to show how it ended.
    fn close_idle_dialog(&mut self) {
        if !self.dialog.as_ref().is_some_and(Dialog::is_running) {
            self.dialog = None;
            self.dialog_subscription = None;
        }
    }

    fn show_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        match self.spaces.get(index).map(|s| s.id.clone()) {
            Some(id) => self.show_space(&id, window, cx),
            None => self.info(format!("there is no space tab {}", index + 1), cx),
        }
    }

    fn on_close_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.view {
            View::Home => window.remove_window(),
            View::Space(_) => self.close_focused_pane(window, cx),
        }
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> TabBar {
        let active = match &self.view {
            View::Space(id) => Some(id),
            View::Home => None,
        };
        let tabs: Vec<TabItem> = self
            .spaces
            .iter()
            .map(|space| {
                let repos = space.repos.len();
                let running = self
                    .tabs
                    .get(&space.id)
                    .is_some_and(|tab| tab.agent_running(cx));
                TabItem::new(space.id.as_str().to_owned(), space.title.clone())
                    .badge(if repos == 1 {
                        "1 repo".to_owned()
                    } else {
                        format!("{repos} repos")
                    })
                    .active(active == Some(&space.id))
                    .running(running)
            })
            .collect();
        let shape = self.active_tab().map(|tab| layout_shape(tab.space.shape));
        let entity = cx.entity().downgrade();
        let (home, select, reshape, ship, discard) = (
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity,
        );
        TabBar::new()
            .home_active(active.is_none())
            .tabs(tabs)
            .active_shape(shape)
            .on_home(move |window, cx| {
                let _ = home.update(cx, |this, cx| this.show_home(window, cx));
            })
            .on_select(move |id, window, cx| {
                let id = SpaceId::new(id.to_string());
                let _ = select.update(cx, |this, cx| this.show_space(&id, window, cx));
            })
            .on_shape(move |shape, window, cx| {
                let _ = reshape.update(cx, |this, cx| this.reshape(shape, window, cx));
            })
            .on_ship(move |window, cx| {
                let _ = ship.update(cx, |this, cx| this.open_ship(window, cx));
            })
            .on_discard(move |window, cx| {
                let _ = discard.update(cx, |this, cx| this.open_discard(window, cx));
            })
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> StatusBar {
        let entity = cx.entity().downgrade();
        let mut bar = StatusBar::new()
            .banner(self.banner.clone())
            .on_dismiss_banner(move |_, cx| {
                let _ = entity.update(cx, |this, cx| this.dismiss_banner(cx));
            });
        match self.active_tab() {
            Some(tab) => {
                let space = &tab.space;
                bar = bar
                    .space(space.id.as_str().to_owned())
                    .mise_env(space.mise_env().to_owned())
                    .port(space.port)
                    .branch(space.branch())
                    .shape(layout_shape(space.shape))
                    .zoomed(tab.layout.tree().is_zoomed())
                    .hints(space_key_hints());
            }
            None => {
                bar = bar.hints(home_key_hints());
                if let Some(busy) = &self.busy {
                    bar = bar.space(format!("{busy}…"));
                }
            }
        }
        bar
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let body = match self.active_tab() {
            Some(tab) => self.render_space(tab, cx),
            None => self.render_home(window, cx),
        };
        let dialog = self.render_dialog(cx);
        div()
            .key_context(WORKSPACE_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &CloseWindow, window, _| window.remove_window()))
            .on_action(
                cx.listener(|this, _: &ClosePane, window, cx| this.on_close_pane(window, cx)),
            )
            .on_action(cx.listener(|this, _: &SplitRight, window, cx| {
                this.split(SplitAxis::Horizontal, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SplitDown, window, cx| {
                this.split(SplitAxis::Vertical, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusLeft, window, cx| {
                this.move_focus(FocusDirection::Left, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusRight, window, cx| {
                this.move_focus(FocusDirection::Right, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusUp, window, cx| {
                this.move_focus(FocusDirection::Up, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusDown, window, cx| {
                this.move_focus(FocusDirection::Down, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &FocusNext, window, cx| this.cycle_focus(true, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &FocusPrev, window, cx| this.cycle_focus(false, window, cx)),
            )
            .on_action(cx.listener(|this, _: &ResizeLeft, window, cx| {
                this.resize_focused(SplitAxis::Horizontal, -1.0, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ResizeRight, window, cx| {
                this.resize_focused(SplitAxis::Horizontal, 1.0, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ResizeUp, window, cx| {
                this.resize_focused(SplitAxis::Vertical, -1.0, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ResizeDown, window, cx| {
                this.resize_focused(SplitAxis::Vertical, 1.0, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleZoom, window, cx| this.toggle_zoom(window, cx)))
            .on_action(cx.listener(|this, _: &ShowHome, window, cx| this.show_home(window, cx)))
            .on_action(cx.listener(|this, action: &ActivateTab, window, cx| {
                this.show_tab(action.0, window, cx)
            }))
            .on_action(cx.listener(|this, _: &LaunchDraft, window, cx| {
                if this.view == View::Home && this.draft.is_some() {
                    this.launch_draft(window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(
                cx.listener(|this, _: &DismissDialog, window, cx| this.dismiss_dialog(window, cx)),
            )
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.bg)
            .text_color(theme.text)
            .text_size(Theme::TEXT_MD)
            .child(self.render_tab_bar(cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(body)
                    .children(dialog),
            )
            .child(self.render_status_bar(cx))
    }
}

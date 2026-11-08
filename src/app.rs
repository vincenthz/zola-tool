//! Application state and the input state machine driving the TUI.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use crate::assist::{self, Action, Assistant, Update};
use crate::date::Date;
use crate::git::{self, GitState};
use crate::images::Images;
use crate::post::{self, Post, Result};

/// How many rows a page-up / page-down jumps in the viewer.
const PAGE_STEP: u16 = 10;

/// Where `zola serve` listens (its own default).
const SERVE_INTERFACE: &str = "127.0.0.1";
const SERVE_PORT: u16 = 1111;

/// A running `zola serve` child process started by the tool.
struct Server {
    child: Child,
    url: String,
}

/// What the text-input overlay is collecting.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    NewTitle,
    Rename,
    Commit,
}

/// Active text-input overlay state.
pub struct InputState {
    pub kind: InputKind,
    pub prompt: String,
    pub buffer: String,
}

/// A pending action awaiting y/n confirmation.
#[derive(Clone)]
pub enum ConfirmAction {
    DeletePost { index: usize, filename: String },
    Push,
}

/// Confirmation-overlay state.
pub struct ConfirmState {
    pub action: ConfirmAction,
}

/// Markdown-preview screen state.
pub struct ViewState {
    pub title: String,
    pub filename: String,
    pub path: PathBuf,
    pub body: String,
    pub scroll: u16,
}

/// Calendar date-picker overlay state.
pub struct DateState {
    pub path: PathBuf,
    pub filename: String,
    pub date: Date,
}

/// Where an assistant request is at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssistPhase {
    /// Before any text: the model loading, the post being read.
    Waiting(String),
    Thinking,
    Writing,
    /// Finished, with a summary (tokens, time).
    Done(String),
    Failed(String),
}

/// Assistant screen state: one request about one post, and its answer.
pub struct AssistState {
    pub action: Action,
    pub title: String,
    pub filename: String,
    pub path: PathBuf,
    /// The request's id, to match the model's updates.
    pub id: u64,
    pub phase: AssistPhase,
    pub thinking: String,
    pub answer: String,
    pub scroll: u16,
    /// Keep the end of the text in view as it grows.
    pub follow: bool,
    /// Asking to confirm a draft replacing the post's body.
    pub confirm_replace: bool,
}

impl AssistState {
    pub fn running(&self) -> bool {
        matches!(
            self.phase,
            AssistPhase::Waiting(_) | AssistPhase::Thinking | AssistPhase::Writing
        )
    }
}

/// Which screen / overlay is currently active.
pub enum Mode {
    List,
    View(ViewState),
    Input(InputState),
    Confirm(ConfirmState),
    DatePicker(DateState),
    /// Choosing what the assistant should do with the selected post.
    AssistMenu,
    Assist(AssistState),
    Help,
}

pub struct App {
    pub content_dir: PathBuf,
    pub posts: Vec<Post>,
    pub selected: usize,
    pub mode: Mode,
    pub status: String,
    pub should_quit: bool,
    /// Set when the user asks to edit; the main loop performs the terminal
    /// suspend + editor launch and clears it.
    pub pending_edit: Option<PathBuf>,
    /// A `zola serve` process, while running.
    server: Option<Server>,
    /// The git repository root, if the blog is under version control.
    git_repo: Option<PathBuf>,
    /// The writing assistant; its model is loaded by the first request.
    assistant: Assistant,
    /// The assistant's model and engine, once loaded.
    pub model_info: Option<String>,
    /// Inline images in the preview, when the terminal shows them.
    pub images: Option<Images>,
}

impl App {
    pub fn new(content_dir: PathBuf) -> Result<Self> {
        let git_repo = git::repo_root(&content_dir);
        let mut posts = post::discover(&content_dir)?;
        let count = posts.len();
        let mut app = Self {
            content_dir,
            posts: Vec::new(),
            selected: 0,
            mode: Mode::List,
            status: format!("{count} post(s) loaded"),
            should_quit: false,
            pending_edit: None,
            server: None,
            git_repo,
            assistant: Assistant::default(),
            model_info: None,
            images: None,
        };
        app.fill_git(&mut posts);
        app.posts = posts;
        Ok(app)
    }

    /// Whether the blog is under git version control.
    pub fn is_git_repo(&self) -> bool {
        self.git_repo.is_some()
    }

    /// Annotate posts with their git tracking state (no-op outside a repo).
    fn fill_git(&self, posts: &mut [Post]) {
        let Some(root) = &self.git_repo else {
            return;
        };
        let Ok(map) = git::statuses(root) else {
            return;
        };
        // Canonicalize both sides so symlinked/relative roots still match.
        let canon: std::collections::HashMap<PathBuf, GitState> = map
            .into_iter()
            .map(|(p, s)| (p.canonicalize().unwrap_or(p), s))
            .collect();
        for post in posts.iter_mut() {
            let key = post
                .path
                .canonicalize()
                .unwrap_or_else(|_| post.path.clone());
            post.git = Some(canon.get(&key).copied().unwrap_or(GitState::Tracked));
        }
    }

    /// Re-scan the content directory, keeping the selection on `keep` if given
    /// (matched by filename), otherwise clamping the current index.
    pub fn reload(&mut self, keep: Option<&str>) {
        match post::discover(&self.content_dir) {
            Ok(mut posts) => {
                self.fill_git(&mut posts);
                self.posts = posts;
                if let Some(name) = keep
                    && let Some(idx) = self.posts.iter().position(|p| p.filename == name)
                {
                    self.selected = idx;
                }
                self.clamp_selection();
            }
            Err(e) => self.status = format!("reload failed: {e}"),
        }
    }

    fn clamp_selection(&mut self) {
        if self.posts.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.posts.len() {
            self.selected = self.posts.len() - 1;
        }
    }

    pub fn selected_post(&self) -> Option<&Post> {
        self.posts.get(self.selected)
    }

    /// Is a preview server currently running?
    pub fn is_serving(&self) -> bool {
        self.server.is_some()
    }

    /// The preview server URL, if running.
    pub fn server_url(&self) -> Option<&str> {
        self.server.as_ref().map(|s| s.url.as_str())
    }

    /// Reap the server if it has exited on its own (e.g. port already in use),
    /// so the UI reflects reality. Call once per event-loop tick.
    pub fn poll_server(&mut self) {
        if let Some(server) = &mut self.server
            && matches!(server.child.try_wait(), Ok(Some(_)))
        {
            self.server = None;
            self.status = "zola serve exited (see the terminal after quitting)".into();
        }
    }

    /// Start `zola serve` (and open a browser) if stopped, or stop it if running.
    pub fn toggle_server(&mut self) {
        if let Some(mut server) = self.server.take() {
            let _ = server.child.kill();
            let _ = server.child.wait();
            self.status = "stopped zola serve".into();
            return;
        }
        match self.start_server() {
            Ok(url) => match open_in_browser(&url) {
                Ok(()) => self.status = format!("serving at {url} — press s to stop"),
                Err(e) => self.status = format!("serving at {url} (couldn't open browser: {e})"),
            },
            Err(e) => self.status = format!("zola serve failed: {e}"),
        }
    }

    fn start_server(&mut self) -> Result<String> {
        let blog_root = self
            .content_dir
            .parent()
            .ok_or("cannot determine blog root from the content directory")?;

        // zola prints build output continuously; send it to a log file rather
        // than let it corrupt the alternate screen.
        let log_path = std::env::temp_dir().join("blog-tool-zola.log");
        let log = std::fs::File::create(&log_path)?;
        let log_err = log.try_clone()?;

        let child = Command::new("zola")
            .arg("serve")
            .args(["--interface", SERVE_INTERFACE])
            .args(["--port", &SERVE_PORT.to_string()])
            .current_dir(blog_root)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .spawn()
            .map_err(|e| format!("could not run `zola` ({e}); is it installed and on PATH?"))?;

        let url = format!("http://{SERVE_INTERFACE}:{SERVE_PORT}");
        self.server = Some(Server {
            child,
            url: url.clone(),
        });
        Ok(url)
    }

    /// Stop the preview server if running, and the assistant's request. Call on
    /// exit so nothing lingers.
    pub fn shutdown(&mut self) {
        if let Mode::Assist(state) = &self.mode {
            self.assistant.cancel(state.id);
        }
        if let Some(mut server) = self.server.take() {
            let _ = server.child.kill();
            let _ = server.child.wait();
        }
    }

    fn select_next(&mut self) {
        if !self.posts.is_empty() {
            self.selected = (self.selected + 1) % self.posts.len();
        }
    }

    fn select_prev(&mut self) {
        if !self.posts.is_empty() {
            self.selected = (self.selected + self.posts.len() - 1) % self.posts.len();
        }
    }

    /// Re-read the previewed post from disk (e.g. after editing it).
    pub fn refresh_view_body(&mut self) {
        if let Mode::View(view) = &mut self.mode
            && let Ok(body) = post::read_body(&view.path)
        {
            view.body = body;
        }
    }

    fn open_view(&mut self) {
        let Some(post) = self.selected_post() else {
            self.status = "no post to view".into();
            return;
        };
        match post::read_body(&post.path) {
            Ok(body) => {
                self.mode = Mode::View(ViewState {
                    title: post.title.clone(),
                    filename: post.filename.clone(),
                    path: post.path.clone(),
                    body,
                    scroll: 0,
                });
            }
            Err(e) => self.status = format!("open failed: {e}"),
        }
    }

    /// Flip the selected post between draft and published.
    fn toggle_draft(&mut self) {
        let Some(post) = self.selected_post() else {
            self.status = "no post selected".into();
            return;
        };
        let target = !post.draft;
        let path = post.path.clone();
        let name = post.filename.clone();
        match post::set_draft(&path, target) {
            Ok(()) => {
                self.reload(Some(&name));
                self.status = format!(
                    "{name} is now {}",
                    if target { "a draft" } else { "published" }
                );
            }
            Err(e) => self.status = format!("could not change draft state: {e}"),
        }
    }

    /// Open the calendar picker to change the selected post's date. Starts from
    /// the post's current date (or today if it can't be parsed).
    fn start_date_edit(&mut self) {
        let Some(post) = self.selected_post() else {
            self.status = "no post selected".into();
            return;
        };
        let date = Date::parse(&post.date).unwrap_or_else(Date::today);
        self.mode = Mode::DatePicker(DateState {
            path: post.path.clone(),
            filename: post.filename.clone(),
            date,
        });
    }

    fn handle_datepicker_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Enter => return self.commit_date(),
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::List;
                self.status = "cancelled".into();
                return;
            }
            _ => {}
        }
        let Mode::DatePicker(state) = &mut self.mode else {
            return;
        };
        let d = state.date;
        state.date = match code {
            KeyCode::Left | KeyCode::Char('h') => d.add_days(-1),
            KeyCode::Right | KeyCode::Char('l') => d.add_days(1),
            KeyCode::Up | KeyCode::Char('k') => d.add_days(-7),
            KeyCode::Down | KeyCode::Char('j') => d.add_days(7),
            KeyCode::PageUp => d.add_months(-1),
            KeyCode::PageDown => d.add_months(1),
            KeyCode::Char('[') => d.add_years(-1),
            KeyCode::Char(']') => d.add_years(1),
            KeyCode::Char('t') | KeyCode::Char('T') => Date::today(),
            _ => d,
        };
    }

    fn commit_date(&mut self) {
        let Mode::DatePicker(state) = &self.mode else {
            return;
        };
        let path = state.path.clone();
        let filename = state.filename.clone();
        let iso = state.date.to_iso();
        match post::set_date(&path, &iso) {
            Ok(()) => {
                self.reload(Some(&filename));
                self.status = format!("{filename} date set to {iso}");
            }
            Err(e) => self.status = format!("could not set date: {e}"),
        }
        self.mode = Mode::List;
    }

    /// Open a commit-message prompt for the selected post (git repos only).
    fn start_commit(&mut self) {
        if !self.is_git_repo() {
            self.status = "not a git repository".into();
            return;
        }
        let Some(post) = self.selected_post() else {
            self.status = "no post selected".into();
            return;
        };
        let default = match post.git {
            Some(GitState::Untracked) => format!("add post: {}", post.title),
            Some(GitState::Modified) => format!("update post: {}", post.title),
            _ => {
                self.status = format!("no changes to commit for {}", post.filename);
                return;
            }
        };
        self.mode = Mode::Input(InputState {
            kind: InputKind::Commit,
            prompt: "Commit message".into(),
            buffer: default,
        });
    }

    /// Ask to confirm a push/deploy to the remote (git repos only).
    fn start_push(&mut self) {
        if !self.is_git_repo() {
            self.status = "not a git repository".into();
            return;
        }
        self.mode = Mode::Confirm(ConfirmState {
            action: ConfirmAction::Push,
        });
    }

    /// Is the assistant working on the request on screen? The main loop then
    /// wakes often to show its progress.
    pub fn is_assisting(&self) -> bool {
        matches!(&self.mode, Mode::Assist(state) if state.running())
    }

    fn open_assist_menu(&mut self) {
        if !assist::AVAILABLE {
            self.status =
                "the assistant needs a build with biv: cargo build --release --features biv".into();
        } else if self.selected_post().is_none() {
            self.status = "no post selected".into();
        } else {
            self.mode = Mode::AssistMenu;
        }
    }

    fn handle_assist_menu_key(&mut self, code: KeyCode) {
        let action = match code {
            KeyCode::Char('r') => Action::Review,
            KeyCode::Char('p') => Action::Plan,
            KeyCode::Char('d') => Action::Draft,
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::List;
                self.status = "cancelled".into();
                return;
            }
            _ => return,
        };
        let Some(post) = self.selected_post().cloned() else {
            self.mode = Mode::List;
            return;
        };
        self.start_assist(action, post.title, post.filename, post.path);
    }

    /// Send the post as it is on disk to the assistant, and show its answer.
    fn start_assist(&mut self, action: Action, title: String, filename: String, path: PathBuf) {
        let body = match post::read_body(&path) {
            Ok(body) => body,
            Err(e) => {
                self.mode = Mode::List;
                self.status = format!("open failed: {e}");
                return;
            }
        };
        if action == Action::Review && !assist::has_content(&body) {
            self.mode = Mode::List;
            self.status =
                format!("{filename} is empty: nothing to review yet (try a plan or a draft)");
            return;
        }
        match self.assistant.start(action, &title, &body) {
            Ok(id) => {
                // the screen's status line is about this request from now on
                self.status.clear();
                let waiting = match &self.model_info {
                    Some(_) => "reading the post…".to_string(),
                    None => "starting the model…".to_string(),
                };
                self.mode = Mode::Assist(AssistState {
                    action,
                    title,
                    filename,
                    path,
                    id,
                    phase: AssistPhase::Waiting(waiting),
                    thinking: String::new(),
                    answer: String::new(),
                    scroll: 0,
                    follow: true,
                    confirm_replace: false,
                });
            }
            Err(e) => {
                self.mode = Mode::List;
                self.status = format!("assistant: {e}");
            }
        }
    }

    /// Take in what the model reported since the last tick. Call once per
    /// event-loop tick.
    pub fn poll_assist(&mut self) {
        for update in self.assistant.poll() {
            self.apply_update(update);
        }
    }

    fn apply_update(&mut self, update: Update) {
        if let Update::Loaded(info) = update {
            self.model_info = Some(info);
            return;
        }
        // updates about a request that is no longer on screen are dropped
        let Mode::Assist(state) = &mut self.mode else {
            return;
        };
        match update {
            Update::Loaded(_) => {}
            Update::Waiting { id, what } if id == state.id => {
                state.phase = AssistPhase::Waiting(what);
            }
            Update::Progress {
                id,
                thinking,
                answer,
            } if id == state.id => {
                state.phase = if answer.is_empty() {
                    AssistPhase::Thinking
                } else {
                    AssistPhase::Writing
                };
                state.thinking = thinking;
                state.answer = answer;
            }
            Update::Done {
                id,
                answer,
                summary,
            } if id == state.id => {
                state.answer = answer;
                state.phase = AssistPhase::Done(summary);
            }
            Update::Failed { id, error } if id == state.id => {
                state.phase = AssistPhase::Failed(error);
            }
            _ => {}
        }
    }

    fn handle_assist_key(&mut self, code: KeyCode) {
        let Mode::Assist(state) = &mut self.mode else {
            return;
        };
        if state.confirm_replace {
            state.confirm_replace = false;
            if matches!(code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                self.apply_assist();
            } else {
                self.status = "kept the post as it is".into();
            }
            return;
        }
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if state.running() {
                    self.assistant.cancel(state.id);
                    self.status = format!("{} cancelled", state.action.label().to_lowercase());
                }
                self.mode = Mode::List;
            }
            KeyCode::Down | KeyCode::Char('j') => state.scroll = state.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => {
                state.follow = false;
                state.scroll = state.scroll.saturating_sub(1);
            }
            KeyCode::PageDown | KeyCode::Char(' ') => {
                state.scroll = state.scroll.saturating_add(PAGE_STEP)
            }
            KeyCode::PageUp => {
                state.follow = false;
                state.scroll = state.scroll.saturating_sub(PAGE_STEP);
            }
            KeyCode::Home | KeyCode::Char('g') => {
                state.follow = false;
                state.scroll = 0;
            }
            // The exact bottom is clamped against the wrapped height at render.
            KeyCode::End | KeyCode::Char('G') => {
                state.follow = true;
                state.scroll = u16::MAX;
            }
            KeyCode::Char('r') if !state.running() => {
                let (action, title, filename, path) = (
                    state.action,
                    state.title.clone(),
                    state.filename.clone(),
                    state.path.clone(),
                );
                self.start_assist(action, title, filename, path);
            }
            KeyCode::Char('a')
                if state.action.applies() && matches!(state.phase, AssistPhase::Done(_)) =>
            {
                // a draft replaces the body: ask first unless there is nothing to lose
                let has_body = post::read_body(&state.path).is_ok_and(|b| assist::has_content(&b));
                if state.action == Action::Draft && has_body {
                    state.confirm_replace = true;
                } else {
                    self.apply_assist();
                }
            }
            _ => {}
        }
    }

    /// Write the finished plan or draft into its post, and go back to the list.
    fn apply_assist(&mut self) {
        let Mode::Assist(state) = &self.mode else {
            return;
        };
        let text = assist::clean_output(&state.answer);
        let filename = state.filename.clone();
        let (result, done) = match state.action {
            Action::Plan => (
                post::append_to_body(&state.path, &format!("## Plan\n\n{text}")),
                "plan appended to",
            ),
            Action::Draft => (post::replace_body(&state.path, &text), "draft written to"),
            Action::Review => return,
        };
        match result {
            Ok(()) => {
                self.reload(Some(&filename));
                self.status = format!("{done} {filename}");
                self.mode = Mode::List;
            }
            Err(e) => self.status = format!("could not write {filename}: {e}"),
        }
    }

    /// Route a key press to the handler for the current mode.
    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        match &self.mode {
            Mode::List => self.handle_list_key(code, mods),
            Mode::View(_) => self.handle_view_key(code),
            Mode::Input(_) => self.handle_input_key(code),
            Mode::Confirm(_) => self.handle_confirm_key(code),
            Mode::DatePicker(_) => self.handle_datepicker_key(code),
            Mode::AssistMenu => self.handle_assist_menu_key(code),
            Mode::Assist(_) => self.handle_assist_key(code),
            Mode::Help => self.mode = Mode::List,
        }
    }

    fn handle_view_key(&mut self, code: KeyCode) {
        // 'e' edits the previewed post; the main loop refreshes the body after.
        if let KeyCode::Char('e') = code {
            if let Mode::View(view) = &self.mode {
                self.pending_edit = Some(view.path.clone());
            }
            return;
        }
        let Mode::View(view) = &mut self.mode else {
            return;
        };
        match code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => self.mode = Mode::List,
            KeyCode::Down | KeyCode::Char('j') => view.scroll = view.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => view.scroll = view.scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => {
                view.scroll = view.scroll.saturating_add(PAGE_STEP)
            }
            KeyCode::PageUp => view.scroll = view.scroll.saturating_sub(PAGE_STEP),
            KeyCode::Home | KeyCode::Char('g') => view.scroll = 0,
            // The exact bottom is clamped against the wrapped height at render.
            KeyCode::End | KeyCode::Char('G') => view.scroll = u16::MAX,
            _ => {}
        }
    }

    fn handle_list_key(&mut self, code: KeyCode, _mods: KeyModifiers) {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Down | KeyCode::Char('j') => self.select_next(),
            KeyCode::Up | KeyCode::Char('k') => self.select_prev(),
            KeyCode::Char('g') | KeyCode::Home => self.selected = 0,
            KeyCode::Char('G') | KeyCode::End => {
                if !self.posts.is_empty() {
                    self.selected = self.posts.len() - 1;
                }
            }
            KeyCode::Enter | KeyCode::Char('v') => self.open_view(),
            KeyCode::Char('e') => {
                if let Some(p) = self.selected_post() {
                    self.pending_edit = Some(p.path.clone());
                } else {
                    self.status = "no post to edit".into();
                }
            }
            KeyCode::Char('n') => {
                self.mode = Mode::Input(InputState {
                    kind: InputKind::NewTitle,
                    prompt: "New post title".into(),
                    buffer: String::new(),
                });
            }
            KeyCode::Char('r') => {
                if let Some(p) = self.selected_post() {
                    self.mode = Mode::Input(InputState {
                        kind: InputKind::Rename,
                        prompt: "Rename file to".into(),
                        buffer: p.filename.clone(),
                    });
                } else {
                    self.status = "no post to rename".into();
                }
            }
            KeyCode::Char('d') => {
                if let Some(p) = self.selected_post() {
                    self.mode = Mode::Confirm(ConfirmState {
                        action: ConfirmAction::DeletePost {
                            index: self.selected,
                            filename: p.filename.clone(),
                        },
                    });
                } else {
                    self.status = "no post to delete".into();
                }
            }
            KeyCode::Char('p') => self.toggle_draft(),
            KeyCode::Char('t') => self.start_date_edit(),
            KeyCode::Char('c') => self.start_commit(),
            KeyCode::Char('P') => self.start_push(),
            KeyCode::Char('s') => self.toggle_server(),
            KeyCode::Char('a') => self.open_assist_menu(),
            KeyCode::Char('?') => self.mode = Mode::Help,
            _ => {}
        }
    }

    fn handle_input_key(&mut self, code: KeyCode) {
        let Mode::Input(input) = &mut self.mode else {
            return;
        };
        match code {
            KeyCode::Esc => {
                self.mode = Mode::List;
                self.status = "cancelled".into();
            }
            KeyCode::Backspace => {
                input.buffer.pop();
            }
            KeyCode::Enter => {
                let kind = input.kind;
                let value = input.buffer.clone();
                self.commit_input(kind, value);
            }
            KeyCode::Char(c) if !c.is_control() => input.buffer.push(c),
            _ => {}
        }
    }

    fn commit_input(&mut self, kind: InputKind, value: String) {
        match kind {
            InputKind::NewTitle => match post::create(&self.content_dir, &value) {
                Ok(path) => {
                    let name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default()
                        .to_string();
                    self.reload(Some(&name));
                    self.status = format!("created {name}");
                    self.mode = Mode::List;
                }
                Err(e) => self.status = format!("create failed: {e}"),
            },
            InputKind::Rename => {
                let Some(post) = self.selected_post().cloned() else {
                    self.mode = Mode::List;
                    return;
                };
                match post::rename(&post, &value) {
                    Ok(path) => {
                        let name = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or_default()
                            .to_string();
                        self.reload(Some(&name));
                        self.status = format!("renamed to {name}");
                        self.mode = Mode::List;
                    }
                    Err(e) => self.status = format!("rename failed: {e}"),
                }
            }
            InputKind::Commit => {
                let message = value.trim().to_string();
                if message.is_empty() {
                    self.status = "commit message is empty".into();
                    return; // keep the overlay open
                }
                let (Some(root), Some(post)) =
                    (self.git_repo.clone(), self.selected_post().cloned())
                else {
                    self.mode = Mode::List;
                    return;
                };
                match git::add_and_commit(&root, &[&post.path], &message) {
                    Ok(()) => {
                        let name = post.filename.clone();
                        self.reload(Some(&name));
                        self.status = format!("committed {name}");
                    }
                    Err(e) => self.status = format!("commit failed: {e}"),
                }
                self.mode = Mode::List;
            }
        }
    }

    fn handle_confirm_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => self.confirm_yes(),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.mode = Mode::List;
                self.status = "cancelled".into();
            }
            _ => {}
        }
    }

    fn confirm_yes(&mut self) {
        let action = match &self.mode {
            Mode::Confirm(c) => c.action.clone(),
            _ => return,
        };
        match action {
            ConfirmAction::DeletePost { index, .. } => {
                if let Some(post) = self.posts.get(index).cloned() {
                    match post::delete(&post) {
                        Ok(()) => {
                            self.reload(None);
                            self.status = format!("deleted {}", post.filename);
                        }
                        Err(e) => self.status = format!("delete failed: {e}"),
                    }
                }
            }
            ConfirmAction::Push => match self.git_repo.clone() {
                Some(root) => match git::push(&root) {
                    Ok(summary) => self.status = format!("deployed — {summary}"),
                    Err(e) => self.status = format!("push failed: {e}"),
                },
                None => self.status = "not a git repository".into(),
            },
        }
        self.mode = Mode::List;
    }
}

/// The platform command that opens `url` in the default browser.
fn browser_command(url: &str) -> (&'static str, Vec<String>) {
    if cfg!(target_os = "macos") {
        ("open", vec![url.to_string()])
    } else if cfg!(target_os = "windows") {
        // `start` is a cmd builtin; the empty title arg avoids quoting pitfalls.
        (
            "cmd",
            vec!["/C".into(), "start".into(), String::new(), url.to_string()],
        )
    } else {
        ("xdg-open", vec![url.to_string()])
    }
}

/// Open `url` in the default browser (best effort, non-blocking).
fn open_in_browser(url: &str) -> Result<()> {
    let (program, args) = browser_command(url);
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    #[test]
    fn browser_command_matches_platform() {
        let (program, args) = browser_command("http://127.0.0.1:1111");
        assert!(args.iter().any(|a| a == "http://127.0.0.1:1111"));
        let expected = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(target_os = "windows") {
            "cmd"
        } else {
            "xdg-open"
        };
        assert_eq!(program, expected);
    }

    fn scratch_app() -> (PathBuf, App) {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "zola-tool-app-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        post::create(&dir, "Hello World").unwrap();
        (dir.clone(), App::new(dir).unwrap())
    }

    fn press(app: &mut App, c: char) {
        app.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
    }

    fn key(app: &mut App, code: KeyCode) {
        app.handle_key(code, KeyModifiers::NONE);
    }

    #[test]
    fn date_picker_navigates_and_sets_date() {
        let (dir, mut app) = scratch_app();
        // A freshly created post is dated today.
        assert_eq!(app.selected_post().unwrap().date, Date::today().to_iso());

        press(&mut app, 't'); // open the calendar
        assert!(
            matches!(app.mode, Mode::DatePicker(_)),
            "picker should open"
        );

        // Right = +1 day, Down = +7 days -> +8 days from today.
        key(&mut app, KeyCode::Right);
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Enter);

        assert!(matches!(app.mode, Mode::List), "picker closes on confirm");
        let expected = Date::today().add_days(8).to_iso();
        assert_eq!(app.selected_post().unwrap().date, expected);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The assistant screen for the selected post, as `start_assist` opens it,
    /// without a model.
    fn assist_screen(app: &mut App, action: Action) {
        let post = app.selected_post().unwrap().clone();
        app.mode = Mode::Assist(AssistState {
            action,
            title: post.title,
            filename: post.filename,
            path: post.path,
            id: 7,
            phase: AssistPhase::Waiting("loading".into()),
            thinking: String::new(),
            answer: String::new(),
            scroll: 0,
            follow: true,
            confirm_replace: false,
        });
    }

    fn finish(app: &mut App, answer: &str) {
        app.apply_update(Update::Done {
            id: 7,
            answer: answer.into(),
            summary: "10 tokens".into(),
        });
    }

    fn body(app: &App) -> String {
        post::read_body(&app.selected_post().unwrap().path).unwrap()
    }

    #[test]
    fn assist_key_needs_the_feature() {
        let (dir, mut app) = scratch_app();
        press(&mut app, 'a');
        if assist::AVAILABLE {
            assert!(matches!(app.mode, Mode::AssistMenu));
            key(&mut app, KeyCode::Esc);
            assert!(matches!(app.mode, Mode::List));
        } else {
            assert!(matches!(app.mode, Mode::List));
            assert!(app.status.contains("--features biv"), "{}", app.status);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reviewing_an_empty_post_is_refused() {
        let (dir, mut app) = scratch_app();
        let post = app.selected_post().unwrap().clone();
        app.start_assist(Action::Review, post.title, post.filename, post.path);
        assert!(matches!(app.mode, Mode::List));
        assert!(app.status.contains("nothing to review"), "{}", app.status);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn updates_stream_into_the_screen_and_stale_ones_are_dropped() {
        let (dir, mut app) = scratch_app();
        assist_screen(&mut app, Action::Review);
        assert!(app.is_assisting());
        app.apply_update(Update::Loaded("Qwen, Metal".into()));
        assert_eq!(app.model_info.as_deref(), Some("Qwen, Metal"));
        app.apply_update(Update::Progress {
            id: 7,
            thinking: "hmm".into(),
            answer: String::new(),
        });
        let Mode::Assist(state) = &app.mode else {
            panic!()
        };
        assert_eq!(state.phase, AssistPhase::Thinking);
        // a cancelled request's last words
        app.apply_update(Update::Done {
            id: 6,
            answer: "old".into(),
            summary: String::new(),
        });
        app.apply_update(Update::Progress {
            id: 7,
            thinking: "hmm".into(),
            answer: "## Summary".into(),
        });
        let Mode::Assist(state) = &app.mode else {
            panic!()
        };
        assert_eq!(state.phase, AssistPhase::Writing);
        assert_eq!(state.answer, "## Summary");
        finish(&mut app, "## Summary\n\nGood.");
        assert!(!app.is_assisting());
        // a review is not written anywhere
        let before = body(&app);
        press(&mut app, 'a');
        assert!(matches!(app.mode, Mode::Assist(_)));
        assert_eq!(body(&app), before);
        key(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::List));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_plan_is_appended_once_done() {
        let (dir, mut app) = scratch_app();
        assist_screen(&mut app, Action::Plan);
        // not before it is finished
        press(&mut app, 'a');
        assert!(matches!(app.mode, Mode::Assist(_)));
        finish(&mut app, "```markdown\n- intro\n- body\n```");
        press(&mut app, 'a');
        assert!(matches!(app.mode, Mode::List));
        assert!(
            body(&app).ends_with("<!-- more -->\n\n## Plan\n\n- intro\n- body\n"),
            "{}",
            body(&app)
        );
        assert_eq!(app.selected_post().unwrap().title, "Hello World");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_draft_replaces_an_empty_body_without_asking() {
        let (dir, mut app) = scratch_app();
        assist_screen(&mut app, Action::Draft);
        finish(&mut app, "Intro.\n\n<!-- more -->\n\n## One\n");
        press(&mut app, 'a');
        assert!(matches!(app.mode, Mode::List));
        assert_eq!(body(&app), "Intro.\n\n<!-- more -->\n\n## One\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_draft_over_existing_text_needs_confirmation() {
        let (dir, mut app) = scratch_app();
        let path = app.selected_post().unwrap().path.clone();
        post::append_to_body(&path, "my notes").unwrap();

        assist_screen(&mut app, Action::Draft);
        finish(&mut app, "The draft.");
        press(&mut app, 'a');
        let Mode::Assist(state) = &app.mode else {
            panic!()
        };
        assert!(state.confirm_replace);
        press(&mut app, 'n'); // declined: nothing written
        assert!(body(&app).contains("my notes"));
        let Mode::Assist(state) = &app.mode else {
            panic!()
        };
        assert!(!state.confirm_replace);

        press(&mut app, 'a');
        press(&mut app, 'y');
        assert!(matches!(app.mode, Mode::List));
        assert_eq!(body(&app), "The draft.\n");
        assert!(post::read_body(&path).is_ok());
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with("+++\ntitle = \"Hello World\"")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn date_picker_cancel_leaves_date_unchanged() {
        let (dir, mut app) = scratch_app();
        let before = app.selected_post().unwrap().date.clone();

        press(&mut app, 't');
        key(&mut app, KeyCode::PageDown); // move a month
        key(&mut app, KeyCode::Esc); // ...then cancel

        assert!(matches!(app.mode, Mode::List));
        assert_eq!(app.selected_post().unwrap().date, before);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

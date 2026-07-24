//! Application state and the input state machine driving the TUI.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use crate::date::Date;
use crate::git::{self, GitState};
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

/// Which screen / overlay is currently active.
pub enum Mode {
    List,
    View(ViewState),
    Input(InputState),
    Confirm(ConfirmState),
    DatePicker(DateState),
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

    /// Stop the preview server if running. Call on exit so it does not linger.
    pub fn shutdown(&mut self) {
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

    /// Route a key press to the handler for the current mode.
    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        match &self.mode {
            Mode::List => self.handle_list_key(code, mods),
            Mode::View(_) => self.handle_view_key(code),
            Mode::Input(_) => self.handle_input_key(code),
            Mode::Confirm(_) => self.handle_confirm_key(code),
            Mode::DatePicker(_) => self.handle_datepicker_key(code),
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

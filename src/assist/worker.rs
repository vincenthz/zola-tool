//! The biv model on a worker thread. The thread is started by the first
//! request, loads the model then, and answers requests one at a time until the
//! [`Handle`] is dropped.
//!
//! The GGUF, the tokenizer, the model, the engine and the chat borrow from one
//! another, so they are all locals of [`serve`].

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use biv::chat::{Chat, ChatOptions, Event, ReasoningEffort};
use biv::engine::Engine;
use biv::model::Model;
use biv::sampler::{Sampler, SamplerConfig};
use biv::tokenizer::Tokenizer;

use super::{Config, Job, Update};

/// The context of a request: a long post, and the longest answer.
const CONTEXT: usize = 32768;

/// Tokens the chat template adds around the system and user texts.
const TEMPLATE_TOKENS: usize = 96;

/// The shortest time between two [`Update::Progress`] of a request.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

pub struct Handle {
    jobs: Sender<Job>,
    updates: Receiver<Update>,
    /// Requests up to this id are cancelled (ids only grow).
    cancelled: Arc<AtomicU64>,
}

impl Handle {
    pub fn spawn(config: Config) -> Handle {
        let (jobs, job_receiver) = mpsc::channel();
        let (updates, update_receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicU64::new(0));
        let worker = Worker {
            jobs: job_receiver,
            updates,
            cancelled: cancelled.clone(),
        };
        // If the thread cannot start, the job receiver is dropped with the
        // closure, and `send` reports it.
        let _ = std::thread::Builder::new()
            .name("biv".into())
            .spawn(move || run(worker, config));
        Handle {
            jobs,
            updates: update_receiver,
            cancelled,
        }
    }

    pub fn send(&self, job: Job) -> Result<(), String> {
        self.jobs
            .send(job)
            .map_err(|_| "the model thread has stopped".to_string())
    }

    pub fn cancel(&self, id: u64) {
        self.cancelled.fetch_max(id, Ordering::Relaxed);
    }

    pub fn poll(&self) -> Vec<Update> {
        self.updates.try_iter().collect()
    }
}

struct Worker {
    jobs: Receiver<Job>,
    updates: Sender<Update>,
    cancelled: Arc<AtomicU64>,
}

impl Worker {
    fn send(&self, update: Update) {
        // a closed channel means nobody listens any more
        let _ = self.updates.send(update);
    }

    fn cancelled(&self, id: u64) -> bool {
        self.cancelled.load(Ordering::Relaxed) >= id
    }

    /// Answers "cancelled" for `job` if it was cancelled before it was
    /// reached; returns whether it was.
    fn skip_cancelled(&self, job: &Job) -> bool {
        let cancelled = self.cancelled(job.id);
        if cancelled {
            self.send(Update::Failed {
                id: job.id,
                error: "cancelled".into(),
            });
        }
        cancelled
    }
}

/// Waits for a request, loads the model and serves requests with it. A model
/// that fails to load fails that request, and is tried again by the next one.
fn run(worker: Worker, config: Config) {
    while let Ok(first) = worker.jobs.recv() {
        if worker.skip_cancelled(&first) {
            continue;
        }
        let id = first.id;
        let error = match catch_unwind(AssertUnwindSafe(|| serve(first, &worker, &config))) {
            Ok(Ok(())) => return,
            Ok(Err(error)) => error,
            Err(panic) => format!("the model crashed: {}", panic_message(&*panic)),
        };
        worker.send(Update::Failed { id, error });
    }
}

/// Loads the model, answers `first` and the requests after it. Returns once
/// the channel is closed, or the load's error.
fn serve(first: Job, worker: &Worker, config: &Config) -> Result<(), String> {
    init_rayon();
    worker.send(Update::Waiting {
        id: first.id,
        what: format!("loading {}…", config.model),
    });
    let path = biv_cli::hf::resolve(&config.model).map_err(|e| {
        format!(
            "{e} (set ZOLA_TOOL_BIV_MODEL, or download it: hf download {})",
            hf_download_hint(&config.model)
        )
    })?;
    let gguf = biv_cli::open_gguf(&path).map_err(|e| e.to_string())?;
    let tok = Tokenizer::from_gguf(&gguf).map_err(|e| format!("{}: {e}", path.display()))?;
    let model = Model::load(&gguf).map_err(|e| format!("{}: {e}", path.display()))?;
    let engine = make_engine(&model, config.cpu)?;
    let name = gguf
        .get("general.name")
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "model".into());
    worker.send(Update::Loaded(format!("{name}, {}", engine.description())));

    let options = ChatOptions {
        thinking: config.thinking,
        effort: ReasoningEffort::Low,
        context: CONTEXT.min(model.cfg.context_length),
        ..ChatOptions::default()
    };
    let mut chat = Chat::new(engine.as_ref(), &tok, options).map_err(|e| e.to_string())?;

    let mut next = Some(first);
    loop {
        let job = match next.take() {
            Some(job) => job,
            None => match worker.jobs.recv() {
                Ok(job) => job,
                Err(_) => return Ok(()),
            },
        };
        if worker.skip_cancelled(&job) {
            continue;
        }
        let id = job.id;
        let update = match catch_unwind(AssertUnwindSafe(|| {
            answer(&mut chat, &tok, config, job, worker)
        })) {
            Ok(Ok(update)) => update,
            Ok(Err(error)) => Update::Failed { id, error },
            Err(panic) => Update::Failed {
                id,
                error: format!("the model crashed: {}", panic_message(&*panic)),
            },
        };
        worker.send(update);
    }
}

/// One request, in a new session: the prompt, then the answer as it comes.
fn answer(
    chat: &mut Chat<'_>,
    tok: &Tokenizer,
    config: &Config,
    job: Job,
    worker: &Worker,
) -> Result<Update, String> {
    let id = job.id;
    let context = chat.opts.context;
    let prompt_tokens =
        tok.encode(&job.system).len() + tok.encode(&job.user).len() + TEMPLATE_TOKENS;
    // what is left for the answer, at least a quarter of the context
    let room = context.saturating_sub(prompt_tokens);
    if room < context / 4 {
        return Err(format!(
            "the post is too long for the model's context ({prompt_tokens} of {context} tokens)"
        ));
    }
    chat.opts.system = Some(job.system);
    chat.opts.max_tokens = job.max_tokens.min(room);
    // the system block is only sent on the first turn of a session
    chat.reset().map_err(|e| e.to_string())?;
    worker.send(Update::Waiting {
        id,
        what: "reading the post…".into(),
    });

    let mut sampler = Sampler::new(sampler_config(config));
    let mut thinking = String::new();
    let mut reply = String::new();
    let mut last = Instant::now();
    let mut shown = 0;
    let started = Instant::now();
    let mut generating = None;
    let result = chat.send(
        &job.user,
        &mut sampler,
        |event| {
            match event {
                Event::Thinking(t) => thinking.push_str(t),
                Event::Answer(t) => reply.push_str(t),
                Event::PromptDone => generating = Some(Instant::now()),
                _ => return,
            }
            let len = thinking.len() + reply.len();
            if last.elapsed() >= PROGRESS_INTERVAL && len != shown {
                last = Instant::now();
                shown = len;
                worker.send(Update::Progress {
                    id,
                    thinking: thinking.clone(),
                    answer: reply.clone(),
                });
            }
        },
        || worker.cancelled(id),
    );
    if worker.cancelled(id) {
        return Err("cancelled".into());
    }
    let stats = result.map_err(|e| e.to_string())?;
    let writing = generating.map_or(Duration::ZERO, |t| t.elapsed());
    let mut summary = format!(
        "{} tokens read, {} written in {:.0}s ({:.0}s total)",
        stats.prompt_tokens,
        stats.gen_tokens,
        writing.as_secs_f32(),
        started.elapsed().as_secs_f32()
    );
    if stats.truncated {
        summary.push_str(" — cut short at the length limit");
    }
    if reply.trim().is_empty() {
        return Err(if stats.truncated {
            "the model ran out of room while thinking, without answering".into()
        } else {
            "the model gave an empty answer".into()
        });
    }
    Ok(Update::Done {
        id,
        answer: reply,
        summary,
    })
}

/// The model card's recommended settings, for thinking or not.
fn sampler_config(config: &Config) -> SamplerConfig {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let (temperature, top_p) = if config.thinking {
        (0.6, 0.95)
    } else {
        (0.7, 0.8)
    };
    SamplerConfig {
        temperature,
        top_k: 20,
        top_p,
        min_p: 0.0,
        seed,
        ..SamplerConfig::default()
    }
}

fn make_engine<'m>(model: &'m Model<'m>, cpu: bool) -> Result<Box<dyn Engine + 'm>, String> {
    #[cfg(target_os = "macos")]
    if !cpu {
        return Ok(Box::new(
            biv::metal::MetalEngine::new(model, 256).map_err(|e| e.to_string())?,
        ));
    }
    // elsewhere the CPU is the only backend
    let _ = cpu;
    Ok(Box::new(model))
}

/// rayon's global pool runs the CPU compute; it can only be built once.
fn init_rayon() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("biv-{i}"))
            .build_global();
    });
}

/// The `hf download` arguments for an `hf.co/<owner>/<repo>:<quant>` name.
fn hf_download_hint(model: &str) -> String {
    let name = model.strip_prefix("hf.co/").unwrap_or(model);
    match name.rsplit_once(':') {
        Some((repo, quant)) => format!("{repo} --include '*{quant}*'"),
        None => name.to_string(),
    }
}

fn panic_message(panic: &(dyn Any + Send)) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: u64) -> Job {
        Job {
            id,
            system: String::new(),
            user: String::new(),
            max_tokens: 16,
        }
    }

    fn next(handle: &Handle) -> Update {
        handle
            .updates
            .recv_timeout(Duration::from_secs(10))
            .expect("an update")
    }

    #[test]
    fn a_missing_model_fails_the_request_and_is_retried() {
        let config = Config {
            model: "/nonexistent/model.gguf".into(),
            cpu: true,
            thinking: false,
        };
        let handle = Handle::spawn(config);
        for id in [1, 2] {
            handle.send(job(id)).unwrap();
            assert!(matches!(next(&handle), Update::Waiting { id: i, .. } if i == id));
            match next(&handle) {
                Update::Failed { id: i, error } => {
                    assert_eq!(i, id);
                    assert!(error.contains("ZOLA_TOOL_BIV_MODEL"), "{error}");
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn a_request_cancelled_before_it_starts_is_skipped() {
        let config = Config {
            model: "/nonexistent/model.gguf".into(),
            cpu: true,
            thinking: false,
        };
        let handle = Handle::spawn(config);
        handle.cancel(1);
        handle.send(job(1)).unwrap();
        match next(&handle) {
            Update::Failed { id: 1, error } => assert_eq!(error, "cancelled"),
            other => panic!("{other:?}"),
        }
    }

    /// Waits for the end of request `id`, printing its progress; returns the
    /// last update and how many `Progress` came before it.
    fn finish(handle: &Handle, id: u64) -> (Update, usize) {
        let mut progress = 0;
        loop {
            let update = handle
                .updates
                .recv_timeout(Duration::from_secs(600))
                .expect("an update");
            match &update {
                Update::Progress { id: i, .. } if *i == id => progress += 1,
                Update::Done { id: i, .. } | Update::Failed { id: i, .. } if *i == id => {
                    return (update, progress);
                }
                other => eprintln!("{other:?}"),
            }
        }
    }

    /// With a real model: `ZOLA_TOOL_BIV_MODEL=hf.co/unsloth/Qwen3.5-4B-GGUF:Q8_0
    /// cargo test --release --features biv -- --ignored --nocapture`
    #[test]
    #[ignore = "needs a model in the Hugging Face cache"]
    fn a_real_model_plans_and_can_be_cancelled() {
        use crate::assist::{Action, prompt};
        let handle = Handle::spawn(Config::from_env());
        let request = |id, action| {
            let (system, user) = prompt(
                action,
                "Why I moved my blog to Zola",
                "notes: was on jekyll, ruby deps broke every year; zola is one binary; \
                 fast builds; shortcodes; downside: smaller theme ecosystem",
            );
            Job {
                id,
                system,
                user,
                max_tokens: action.max_tokens(),
            }
        };

        handle.send(request(1, Action::Plan)).unwrap();
        let (update, progress) = finish(&handle, 1);
        let Update::Done {
            answer, summary, ..
        } = update
        else {
            panic!("{update:?}")
        };
        eprintln!("--- plan ({summary}, {progress} progress updates)\n{answer}\n---");
        assert!(progress > 0, "the answer should stream");
        assert!(answer.contains("- ") || answer.contains("* "), "not a list");

        // cancelled once it streams
        handle.send(request(2, Action::Draft)).unwrap();
        loop {
            match next(&handle) {
                Update::Progress { id: 2, .. } => break,
                Update::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        }
        let cancelled = Instant::now();
        handle.cancel(2);
        let (update, _) = finish(&handle, 2);
        assert!(matches!(&update, Update::Failed { error, .. } if error == "cancelled"));
        eprintln!("cancelled in {:?}", cancelled.elapsed());
        assert!(cancelled.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn download_hint_names_the_repo_and_quant() {
        assert_eq!(
            hf_download_hint("hf.co/unsloth/Qwen3.8-27B-GGUF:Q4_1"),
            "unsloth/Qwen3.8-27B-GGUF --include '*Q4_1*'"
        );
    }
}

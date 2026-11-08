//! The writing assistant: a local language model (biv) that reviews a post,
//! drafts a plan for it, or drafts the post itself.
//!
//! The prompts and the cleaning of the model's output are plain functions,
//! compiled with or without the `biv` feature. The model itself runs on a
//! worker thread ([`worker`]) only with the feature; without it,
//! [`Assistant::start`] says how to get it.

#[cfg(feature = "biv")]
mod worker;

/// Whether this build has the assistant (the `biv` feature).
pub const AVAILABLE: bool = cfg!(feature = "biv");

/// The model biv's command line uses by default.
#[cfg_attr(not(feature = "biv"), allow(dead_code))]
pub const DEFAULT_MODEL: &str = "hf.co/unsloth/Qwen3.8-27B-GGUF:Q4_1";

/// What the assistant is asked to do with a post.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Feedback on the post as it is.
    Review,
    /// An outline, from the title and the notes in the post.
    Plan,
    /// The whole post body, from the title and the notes or plan.
    Draft,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Action::Review => "Review",
            Action::Plan => "Plan",
            Action::Draft => "Draft",
        }
    }

    /// Whether the result can be written into the post.
    pub fn applies(self) -> bool {
        matches!(self, Action::Plan | Action::Draft)
    }

    fn system(self) -> &'static str {
        match self {
            Action::Review => REVIEW_SYSTEM,
            Action::Plan => PLAN_SYSTEM,
            Action::Draft => DRAFT_SYSTEM,
        }
    }

    /// The longest answer, in tokens (thinking included).
    fn max_tokens(self) -> usize {
        match self {
            Action::Review => 3072,
            Action::Plan => 2048,
            Action::Draft => 6144,
        }
    }
}

const REVIEW_SYSTEM: &str = "\
You are an experienced editor helping an author review a blog post before it is published. \
Read the post and give honest, specific, actionable feedback in Markdown, in these sections:

## Summary
Two or three sentences: what the post says, and how well it gets it across.

## Strengths
What works and should be kept.

## Issues
The most important problems first: structure and flow, clarity, correctness, missing context \
or examples, tone, length.

## Line edits
Concrete fixes: quote the original text, then give the replacement.

Be concise. Do not rewrite the whole post. Answer in the language of the post.";

const PLAN_SYSTEM: &str = "\
You help an author plan a blog post. From its title and the author's notes (which may be rough, \
partial or empty), write an outline: first the main point of the post in one sentence, then its \
sections in order as a nested Markdown bullet list, each with the points, examples or code it \
should cover. Note where the reader's likely questions should be answered. \
Output only the outline: no heading, no preamble, no closing remarks. \
Write in the language of the title and notes.";

const DRAFT_SYSTEM: &str = "\
You are drafting a blog post for its author. From the title and the author's notes or outline, \
write the complete body of the post in Markdown. Follow the outline when there is one, keep the \
facts, code and opinions of the notes, and match the author's voice. \
Start with a short introduction telling the reader what they will get, followed by a line \
containing only `<!-- more -->` (it marks the end of the summary on the site). \
Use `##` headings for the sections. Do not repeat the title as a heading and do not write front \
matter. Output only the body of the post: no preamble, no closing remarks. \
Write in the language of the title and notes.";

/// The system and user messages for `action` on a post.
pub fn prompt(action: Action, title: &str, body: &str) -> (String, String) {
    let content = if has_content(body) {
        format!("<post>\n{}\n</post>", body.trim())
    } else {
        "The post has no content yet.".to_string()
    };
    let ask = match action {
        Action::Review => "Review this post.",
        Action::Plan => "Write the outline of this post.",
        Action::Draft => "Write this post.",
    };
    let user = format!("Title: {}\n\n{content}\n\n{ask}", title.trim());
    (action.system().to_string(), user)
}

/// Whether a post body has anything besides whitespace and HTML comments
/// (such as the `<!-- more -->` of a new post).
pub fn has_content(body: &str) -> bool {
    let mut rest = body;
    while let Some(start) = rest.find("<!--") {
        if !rest[..start].trim().is_empty() {
            return true;
        }
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => return false,
        }
    }
    !rest.trim().is_empty()
}

/// The model's answer, ready to go into a post: trimmed, and without the
/// code fence it is sometimes wrapped in.
pub fn clean_output(text: &str) -> String {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix("```")
        && let Some((info, inner)) = rest.split_once('\n')
        && matches!(info.trim(), "" | "markdown" | "md")
        && let Some(inner) = inner.trim_end().strip_suffix("```")
    {
        return inner.trim().to_string();
    }
    text.to_string()
}

/// A request to the model: one independent turn.
#[cfg_attr(not(feature = "biv"), allow(dead_code))]
pub struct Job {
    pub id: u64,
    pub system: String,
    pub user: String,
    pub max_tokens: usize,
}

/// What the model reports, tagged with the id of the request it is about.
#[cfg_attr(not(feature = "biv"), allow(dead_code))]
#[derive(Debug, Clone)]
pub enum Update {
    /// The model is loaded: its name and engine.
    Loaded(String),
    /// What request `id` waits on (the model loading, the post being read).
    Waiting {
        id: u64,
        what: String,
    },
    /// The text so far: the reasoning, then the answer.
    Progress {
        id: u64,
        thinking: String,
        answer: String,
    },
    /// The whole answer, and how it went (tokens, time).
    Done {
        id: u64,
        answer: String,
        summary: String,
    },
    Failed {
        id: u64,
        error: String,
    },
}

/// Where the model comes from and how it runs, from the environment.
#[cfg_attr(not(feature = "biv"), allow(dead_code))]
#[derive(Debug, Clone)]
pub struct Config {
    /// A GGUF path or an `hf.co/<owner>/<repo>:<quant>` name in the Hugging
    /// Face cache (`ZOLA_TOOL_BIV_MODEL`).
    pub model: String,
    /// Run on the CPU even where Metal is available (`ZOLA_TOOL_BIV_CPU=1`).
    pub cpu: bool,
    /// Let the model reason before answering: slower, sometimes better
    /// (`ZOLA_TOOL_BIV_THINK=1`).
    pub thinking: bool,
}

impl Config {
    #[cfg_attr(not(feature = "biv"), allow(dead_code))]
    pub fn from_env() -> Self {
        let flag = |name| std::env::var(name).is_ok_and(|v| !matches!(v.as_str(), "" | "0"));
        Config {
            model: std::env::var("ZOLA_TOOL_BIV_MODEL")
                .ok()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            cpu: flag("ZOLA_TOOL_BIV_CPU"),
            thinking: flag("ZOLA_TOOL_BIV_THINK"),
        }
    }
}

/// The handle to the model. Nothing is started until the first request; the
/// model is then loaded once and kept until the tool exits.
#[derive(Default)]
pub struct Assistant {
    next_id: u64,
    #[cfg(feature = "biv")]
    worker: Option<worker::Handle>,
}

impl Assistant {
    /// Ask the model to do `action` on a post; returns the request's id.
    pub fn start(&mut self, action: Action, title: &str, body: &str) -> Result<u64, String> {
        self.next_id += 1;
        let (system, user) = prompt(action, title, body);
        let job = Job {
            id: self.next_id,
            system,
            user,
            max_tokens: action.max_tokens(),
        };
        self.send(job)?;
        Ok(self.next_id)
    }

    #[cfg(feature = "biv")]
    fn send(&mut self, job: Job) -> Result<(), String> {
        let worker = self
            .worker
            .get_or_insert_with(|| worker::Handle::spawn(Config::from_env()));
        worker.send(job)
    }

    #[cfg(not(feature = "biv"))]
    fn send(&mut self, _job: Job) -> Result<(), String> {
        Err("built without biv: rebuild with `cargo build --release --features biv`".into())
    }

    /// Stop request `id` (and any before it).
    pub fn cancel(&self, id: u64) {
        #[cfg(feature = "biv")]
        if let Some(worker) = &self.worker {
            worker.cancel(id);
        }
        let _ = id;
    }

    /// The updates received since the last call.
    pub fn poll(&mut self) -> Vec<Update> {
        #[cfg(feature = "biv")]
        if let Some(worker) = &self.worker {
            return worker.poll();
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_bodies_have_no_content() {
        assert!(!has_content(""));
        assert!(!has_content("\n<!-- more -->\n"));
        assert!(!has_content("<!-- a -->  <!-- b -->"));
        assert!(has_content("<!-- more -->\nsome notes"));
        assert!(has_content("notes <!-- more -->"));
        // an unterminated comment hides the rest, as in HTML
        assert!(!has_content("<!-- never closed\ntext"));
    }

    #[test]
    fn prompt_includes_title_and_body() {
        let (system, user) = prompt(Action::Review, " Rust and Haskell ", "Some *notes*.\n");
        assert!(system.contains("## Line edits"));
        assert!(user.starts_with("Title: Rust and Haskell\n"));
        assert!(user.contains("<post>\nSome *notes*.\n</post>"));
        assert!(user.ends_with("Review this post."));
    }

    #[test]
    fn prompt_for_an_empty_post_says_so() {
        let (system, user) = prompt(Action::Plan, "Title", "<!-- more -->\n");
        assert!(system.contains("outline"));
        assert!(user.contains("no content yet"));
        assert!(!user.contains("<post>"));
    }

    #[test]
    fn only_plans_and_drafts_apply() {
        assert!(!Action::Review.applies());
        assert!(Action::Plan.applies());
        assert!(Action::Draft.applies());
    }

    #[test]
    fn output_is_unfenced_and_trimmed() {
        assert_eq!(clean_output("\n  # Hi\n\ntext\n"), "# Hi\n\ntext");
        assert_eq!(clean_output("```markdown\n# Hi\n```\n"), "# Hi");
        assert_eq!(clean_output("```\n- a\n- b\n```"), "- a\n- b");
        // code of another language, or a fence that is not the whole answer, stays
        assert_eq!(
            clean_output("```rust\nfn x() {}\n```"),
            "```rust\nfn x() {}\n```"
        );
        assert_eq!(
            clean_output("```\ncode\n```\nand text"),
            "```\ncode\n```\nand text"
        );
    }

    #[cfg(not(feature = "biv"))]
    #[test]
    fn without_biv_requests_explain_the_feature() {
        let mut assistant = Assistant::default();
        let err = assistant.start(Action::Review, "t", "b").unwrap_err();
        assert!(err.contains("--features biv"));
        assert!(assistant.poll().is_empty());
    }
}

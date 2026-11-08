# blog-tool

A small terminal UI (TUI) for managing the posts of this Zola blog. It operates
on the markdown files in the site's `content/` directory.

## Features

- **List** all posts (date, draft/published status, title), newest first.
- **Preview** a post as rendered markdown directly in the terminal — headings,
  bold/italic/strikethrough, inline code, code blocks, ordered/unordered lists,
  blockquotes, links, horizontal rules and simple tables — with scrolling and a
  scrollbar. Front matter and raw HTML (e.g. `<!-- more -->`) are stripped.
- **Create** a new draft post from a title — generates a `YYYY-MM-DD-slug.md`
  file pre-filled with the standard Zola front matter.
- **Rename** a post's file.
- **Edit** a post in your `$EDITOR` (falls back to `$VISUAL`, then
  `nvim`/`vim`/`vi`/`nano`) — also available from within the preview.
- **Publish / unpublish** — toggle a post's `draft` flag in place (the rest of
  the front matter and body are left untouched).
- **Change date** — pick a post's `date` from an interactive month **calendar**
  (arrows for day/week, PageUp/Down for month, `[`/`]` for year, `t` for today),
  past or future; the list re-sorts accordingly.
- **Delete** a post, behind a confirmation prompt.
- **Serve** the site live with one key — starts `zola serve` in the background
  and opens `http://127.0.0.1:1111` in your browser. Press again to stop; it is
  also stopped automatically when you quit, so nothing is left running. While
  it runs, zola live-reloads as you create/edit posts.
- **Git integration** (only when the blog is a git repo):
  - a **Git** column shows whether each post is `tracked`, `modified`, or
    `untracked`;
  - **commit** the selected post (prompts for a message, prefilled from the
    post's state — "add post: …" for a new file, "update post: …" otherwise);
  - **deploy** by pushing commits to the remote (behind a confirmation).

- **Writing assistant** (optional, see [below](#writing-assistant-biv)): a
  local language model, run by [biv](https://github.com/vincenthz/biv),
  **reviews** a post, drafts a **plan** (outline) for it from its title and
  notes, or drafts the **post** itself. The answer streams into a scrollable
  pane; a plan can then be appended to the post, and a draft can replace its
  body (after a confirmation if the body has text in it).

Section indexes (`_index.md`) and non-markdown files are never touched.

Requires [`zola`](https://www.getzola.org/) on your `PATH` for the serve
feature and `git` for the git features; everything else works without either.

## Build & run

```sh
cd blog-tool
cargo run --release
```

The tool locates the blog root automatically by walking up from the current
directory to find a `config.toml` next to a `content/` folder, so it works from
anywhere inside the repo. You can also point it at a specific directory:

```sh
cargo run --release -- /path/to/blog        # blog root
cargo run --release -- /path/to/blog/content # content dir directly
```

## Writing assistant (biv)

The assistant is a cargo feature, off by default. It pulls the
[biv](https://github.com/vincenthz/biv) inference engine from GitHub (pure
Rust, Metal on macOS, CPU elsewhere; Rust 1.98 or later):

```sh
cargo run --release --features biv
```

Press `a` on a post, then:

| Key | Action | Result |
|-----|--------|--------|
| `r` | **Review**: feedback on the post (summary, strengths, issues, line edits) | shown only |
| `p` | **Plan**: an outline from the title and the notes in the post | `a` appends it under `## Plan` |
| `d` | **Draft**: the whole body, from the title and the notes or plan | `a` replaces the body (front matter kept) |

The post is read from disk when the request starts, so notes typed in
`$EDITOR`, or a plan appended before, are what the model works from. Each
request is one independent turn; `r` in the pane regenerates.

The model is loaded the first time it is used, and stays loaded until the tool
exits. It is looked up in the Hugging Face cache (`$HF_HUB_CACHE`,
`$HF_HOME/hub`, `~/.cache/huggingface/hub`); biv does not download, so get it
first:

```sh
hf download unsloth/Qwen3.8-27B-GGUF Qwen3.8-27B-Q4_1.gguf   # the default, 17.5 GB
```

| Variable | Effect |
|----------|--------|
| `ZOLA_TOOL_BIV_MODEL` | another `hf.co/<owner>/<repo>:<quant>` name, or a `.gguf` path (e.g. `hf.co/unsloth/Qwen3.5-4B-GGUF:Q8_0`, faster and lighter) |
| `ZOLA_TOOL_BIV_CPU=1` | run on the CPU even where Metal is available |
| `ZOLA_TOOL_BIV_THINK=1` | let the model reason before answering (slower, sometimes better) |

On an M2 Max, the default model loads in a few seconds and writes about 25
tokens/s: a plan takes about 30 s, a full draft a minute or two.

## Keys

### Post list

| Key       | Action                                    |
|-----------|-------------------------------------------|
| `↑`/`k`   | move selection up                         |
| `↓`/`j`   | move selection down                       |
| `g` / `G` | jump to first / last                      |
| `⏎` / `v` | preview the selected post (rendered)      |
| `e`       | edit the selected post in `$EDITOR`       |
| `p`       | toggle draft / published                  |
| `t`       | change the post's date (calendar picker)  |
| `n`       | create a new draft post                   |
| `r`       | rename the selected post's file           |
| `d`       | delete the selected post (asks to confirm)|
| `c`       | commit the selected post (git repos only) |
| `P`       | deploy: push commits to remote (git only) |
| `s`       | start/stop `zola serve` + open in browser |
| `a`       | assistant: review / plan / draft (biv)    |
| `?`       | toggle help                               |
| `q`/`Esc` | quit                                      |

### Preview

| Key              | Action                        |
|------------------|-------------------------------|
| `↑`/`↓`, `k`/`j` | scroll one line               |
| `PgUp`/`PgDn`    | scroll one page               |
| `g` / `G`        | jump to top / bottom          |
| `e`              | edit this post in `$EDITOR`   |
| `q`/`Esc`/`⏎`    | back to the list              |

### Assistant pane (biv)

| Key              | Action                                         |
|------------------|------------------------------------------------|
| `↑`/`↓`, `k`/`j` | scroll (follows the end while it writes)       |
| `PgUp`/`PgDn`    | scroll one page                                |
| `g` / `G`        | jump to top / bottom (and follow again)        |
| `a`              | apply: append the plan / replace the body      |
| `r`              | regenerate                                     |
| `q`/`Esc`        | cancel the request, or back to the list        |

### Date picker (calendar)

| Key                | Action                        |
|--------------------|-------------------------------|
| `←`/`→`, `h`/`l`   | previous / next day           |
| `↑`/`↓`, `k`/`j`   | previous / next week          |
| `PgUp`/`PgDn`      | previous / next month         |
| `[` / `]`          | previous / next year          |
| `t`                | jump to today                 |
| `⏎`                | apply the selected date       |
| `q`/`Esc`          | cancel                        |

## Tests

```sh
cargo test
```

Covers front-matter parsing/stripping, slug generation, the
create/rename/delete filesystem operations, the draft-toggle and date
front-matter edits, the calendar date arithmetic (parse/validate, add
days/months/years with rollover and leap-day clamping, weekday), the date
picker's navigate-and-apply flow, git repo detection / status classification /
commit (in a throwaway repo), the markdown renderer (headings, lists, code
blocks, HTML skipping, wrap counting), and headless rendering of each screen —
including the preview and calendar — via ratatui's `TestBackend`, and the
assistant's prompts, output cleaning, apply-to-post edits and screen flow (with
simulated model updates, so no model is needed).

```sh
cargo test --features biv                       # also the biv worker (no model)
ZOLA_TOOL_BIV_MODEL=hf.co/unsloth/Qwen3.5-4B-GGUF:Q8_0 \
  cargo test --release --features biv -- --ignored --nocapture   # with a real model
```

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
| `n`       | create a new draft post                   |
| `r`       | rename the selected post's file           |
| `d`       | delete the selected post (asks to confirm)|
| `c`       | commit the selected post (git repos only) |
| `P`       | deploy: push commits to remote (git only) |
| `s`       | start/stop `zola serve` + open in browser |
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

## Tests

```sh
cargo test
```

Covers front-matter parsing/stripping, slug generation, the
create/rename/delete filesystem operations, the draft-toggle front-matter edit,
git repo detection / status classification / commit (in a throwaway repo), the
markdown renderer (headings, lists, code blocks, HTML skipping, wrap counting),
and headless rendering of each screen — including the preview — via ratatui's
`TestBackend`.

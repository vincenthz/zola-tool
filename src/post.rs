//! Post discovery, front-matter parsing, and filesystem operations.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use crate::date::Date;
use crate::git::GitState;

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// A single blog post backed by a markdown file in `content/`.
#[derive(Clone, Debug)]
pub struct Post {
    pub path: PathBuf,
    pub filename: String,
    pub title: String,
    pub date: String,
    pub draft: bool,
    /// Git tracking state, filled in by the app when the blog is a repo.
    pub git: Option<GitState>,
}

/// Locate the blog's `content/` directory.
///
/// If a path is given on the command line it is used directly (either the blog
/// root, or the content dir itself). Otherwise we walk up from the current
/// directory looking for a `config.toml` next to a `content/` directory, so the
/// tool works no matter which subdirectory it is launched from.
pub fn find_content_dir() -> Result<PathBuf> {
    if let Some(arg) = std::env::args().nth(1) {
        let p = PathBuf::from(&arg);
        if !p.exists() {
            return Err(format!("path does not exist: {}", p.display()).into());
        }
        let nested = p.join("content");
        if nested.is_dir() {
            return Ok(nested);
        }
        if p.is_dir() {
            return Ok(p);
        }
        return Err(format!("not a directory: {}", p.display()).into());
    }

    let mut dir = std::env::current_dir()?;
    loop {
        let content = dir.join("content");
        if content.is_dir() && dir.join("config.toml").is_file() {
            return Ok(content);
        }
        if !dir.pop() {
            break;
        }
    }
    Err("could not locate the blog root (no config.toml next to a content/ directory found in the current path)".into())
}

/// Discover all managed posts in `content/`, sorted newest first.
///
/// Section indexes (`_index.md`), non-markdown files, and subdirectories are
/// ignored — only top-level posts are managed.
pub fn discover(content_dir: &Path) -> Result<Vec<Post>> {
    let mut posts = Vec::new();
    for entry in fs::read_dir(content_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name == "_index.md" || !name.ends_with(".md") {
            continue;
        }
        posts.push(parse_post(&path)?);
    }
    posts.sort_by(|a, b| {
        b.date
            .cmp(&a.date)
            .then_with(|| a.filename.cmp(&b.filename))
    });
    Ok(posts)
}

/// Read a file and extract `title`, `date` and `draft` from its `+++` TOML
/// front matter with a lightweight line scanner (no full TOML parse, so the
/// file is never reformatted).
fn parse_post(path: &Path) -> Result<Post> {
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let content = fs::read_to_string(path)?;

    let mut title = String::new();
    let mut date = String::new();
    let mut draft = false;

    let mut in_front_matter = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "+++" {
            if in_front_matter {
                break; // end of front matter
            }
            in_front_matter = true;
            continue;
        }
        if !in_front_matter {
            // No front matter at all: bail after the first non-delimiter line.
            if !trimmed.is_empty() {
                break;
            }
            continue;
        }
        if let Some(v) = field(trimmed, "title") {
            title = unquote(v);
        } else if let Some(v) = field(trimmed, "date") {
            date = unquote(v);
        } else if let Some(v) = field(trimmed, "draft") {
            draft = v.trim() == "true";
        }
    }

    if title.is_empty() {
        title = filename.trim_end_matches(".md").to_string();
    }

    Ok(Post {
        path: path.to_path_buf(),
        filename,
        title,
        date,
        draft,
        git: None,
    })
}

/// If `line` is `key = value` (top-level, not nested under a `[table]`), return
/// the raw value slice.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(key)?;
    let rest = rest.trim_start();
    let value = rest.strip_prefix('=')?;
    Some(value.trim())
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix('"').unwrap_or(s);
    let s = s.strip_suffix('"').unwrap_or(s);
    s.to_string()
}

/// Build a URL-friendly slug from a post title.
pub fn slugify(title: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !slug.is_empty() {
            slug.push('-');
            prev_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

/// Create a new draft post from a title, returning the new file path.
///
/// The filename is `YYYY-MM-DD-slug.md` and the file is pre-filled with a
/// standard Zola front matter block.
pub fn create(content_dir: &Path, title: &str) -> Result<PathBuf> {
    let title = title.trim();
    if title.is_empty() {
        return Err("title cannot be empty".into());
    }
    let date = today();
    let slug = slugify(title);
    let slug = if slug.is_empty() {
        "untitled".to_string()
    } else {
        slug
    };
    let filename = format!("{date}-{slug}.md");
    let path = content_dir.join(&filename);
    if path.exists() {
        return Err(format!("a post named {filename} already exists").into());
    }

    let body = format!(
        "+++\n\
         title = \"{title}\"\n\
         date = {date}\n\
         draft = true\n\
         [taxonomies]\n\
         tags = []\n\
         categories = []\n\
         +++\n\n\
         <!-- more -->\n",
    );
    fs::write(&path, body)?;
    Ok(path)
}

/// Rename a post's file, returning the new path. `new_name` may omit the `.md`
/// extension.
pub fn rename(post: &Post, new_name: &str) -> Result<PathBuf> {
    let mut new_name = new_name.trim().to_string();
    if new_name.is_empty() {
        return Err("filename cannot be empty".into());
    }
    if new_name.contains('/') || new_name.contains(std::path::MAIN_SEPARATOR) {
        return Err("filename must not contain a path separator".into());
    }
    if !new_name.ends_with(".md") {
        new_name.push_str(".md");
    }
    if new_name == post.filename {
        return Err("new name is identical to the current name".into());
    }
    let parent = post.path.parent().ok_or("post has no parent directory")?;
    let new_path = parent.join(&new_name);
    if new_path.exists() {
        return Err(format!("a post named {new_name} already exists").into());
    }
    fs::rename(&post.path, &new_path)?;
    Ok(new_path)
}

/// Delete a post's file.
pub fn delete(post: &Post) -> Result<()> {
    fs::remove_file(&post.path)?;
    Ok(())
}

/// Set the `draft` flag in a post's front matter, editing only that field and
/// leaving the rest of the file untouched. Inserts the field if absent.
pub fn set_draft(path: &Path, draft: bool) -> Result<()> {
    // Keep the flag just below the date (or title) when it has to be inserted.
    edit_field(
        path,
        "draft",
        &format!("draft = {draft}"),
        &["date", "title"],
    )
}

/// Set the `date` field (a Zola/TOML date literal, `YYYY-MM-DD`) in a post's
/// front matter. The date may be in the past or the future.
pub fn set_date(path: &Path, new_date: &str) -> Result<()> {
    let date = new_date.trim();
    if Date::parse(date).is_none() {
        return Err(format!("invalid date '{date}' (expected YYYY-MM-DD)").into());
    }
    edit_field(path, "date", &format!("date = {date}"), &["title"])
}

/// Replace a top-level front-matter field with `line`, or insert `line` if the
/// field is absent — placing it after the first present field named in
/// `insert_after` (priority order), else right after the opening `+++`.
///
/// Only the front-matter block is touched, and only its top-level region before
/// any `[table]` sub-section, so nested keys of the same name are never hit.
fn edit_field(path: &Path, key: &str, line: &str, insert_after: &[&str]) -> Result<()> {
    let content = fs::read_to_string(path)?;
    let had_trailing_newline = content.ends_with('\n');
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();

    let mut delims = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trim() == "+++")
        .map(|(i, _)| i);
    let start = delims.next().ok_or("post has no front matter")?;
    let end = delims.next().ok_or("post front matter is not terminated")?;

    let region_end = ((start + 1)..end)
        .find(|&i| lines[i].trim_start().starts_with('['))
        .unwrap_or(end);

    if let Some(i) = (start + 1..region_end).find(|&i| field(lines[i].trim(), key).is_some()) {
        lines[i] = line.to_string();
    } else {
        let mut at = start + 1;
        'search: for name in insert_after {
            for (i, l) in lines.iter().enumerate().take(region_end).skip(start + 1) {
                if field(l.trim(), name).is_some() {
                    at = i + 1;
                    break 'search;
                }
            }
        }
        lines.insert(at, line.to_string());
    }

    let mut out = lines.join("\n");
    if had_trailing_newline {
        out.push('\n');
    }
    fs::write(path, out)?;
    Ok(())
}

/// Read a post's markdown body, with the `+++`/`---` front matter stripped so
/// it can be handed to the markdown viewer.
pub fn read_body(path: &Path) -> Result<String> {
    let content = fs::read_to_string(path)?;
    Ok(strip_front_matter(&content).to_string())
}

/// Append `text` to a post's body, after a blank line, leaving the front
/// matter and the existing body as they are.
pub fn append_to_body(path: &Path, text: &str) -> Result<()> {
    let content = fs::read_to_string(path)?;
    let mut out = content.trim_end().to_string();
    out.push_str("\n\n");
    out.push_str(text.trim());
    out.push('\n');
    fs::write(path, out)?;
    Ok(())
}

/// Replace a post's body with `body`, keeping its front matter byte for byte.
pub fn replace_body(path: &Path, body: &str) -> Result<()> {
    let content = fs::read_to_string(path)?;
    let mut out = content[..front_matter_len(&content)].to_string();
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(body.trim());
    out.push('\n');
    fs::write(path, out)?;
    Ok(())
}

/// Return the slice of `content` after the leading front-matter block, or the
/// whole string if there is none.
fn strip_front_matter(content: &str) -> &str {
    content[front_matter_len(content)..].trim_start_matches('\n')
}

/// The length of the leading `+++`/`---` front-matter block, closing
/// delimiter line included; 0 if there is none (or it is not terminated).
fn front_matter_len(content: &str) -> usize {
    let first = content.lines().next().map(str::trim).unwrap_or("");
    let delim = match first {
        "+++" => "+++",
        "---" => "---",
        _ => return 0,
    };
    let mut offset = 0;
    for (i, line) in content.split_inclusive('\n').enumerate() {
        offset += line.len();
        // Skip the opening delimiter; stop past the closing one.
        if i > 0 && line.trim_end_matches(['\n', '\r']).trim() == delim {
            return offset;
        }
    }
    0
}

/// Today's date as `YYYY-MM-DD` (UTC).
fn today() -> String {
    Date::today().to_iso()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique, empty scratch directory. An atomic counter (not a timestamp)
    /// guarantees uniqueness even when tests run in parallel on a coarse clock.
    fn scratch() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "blog-tool-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir); // clear any leftover from a prior run
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn slugify_matches_zola_style() {
        assert_eq!(slugify("Combining Foo and Bar"), "combining-foo-and-bar");
        assert_eq!(
            slugify("Cryptoxide perf (SHA2 / Blake2)"),
            "cryptoxide-perf-sha2-blake2"
        );
        assert_eq!(slugify("  weird---spacing  "), "weird-spacing");
        assert_eq!(slugify("!!!"), "");
    }

    #[test]
    fn parses_toml_front_matter() {
        let dir = scratch();
        let path = dir.join("sample.md");
        fs::write(
            &path,
            "+++\ntitle = \"Hello World\"\ndate = 2021-01-17\ndraft = true\n[taxonomies]\ntags = [\"a\"]\n+++\n\nbody\n",
        )
        .unwrap();
        let post = parse_post(&path).unwrap();
        assert_eq!(post.title, "Hello World");
        assert_eq!(post.date, "2021-01-17");
        assert!(post.draft);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn create_rename_delete_roundtrip() {
        let dir = scratch();

        // discover on an empty dir
        assert!(discover(&dir).unwrap().is_empty());

        // create
        let path = create(&dir, "My First Post").unwrap();
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(name.ends_with("-my-first-post.md"));
        let posts = discover(&dir).unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].title, "My First Post");
        assert!(posts[0].draft);

        // duplicate create is rejected
        assert!(create(&dir, "My First Post").is_err());

        // rename (extension auto-added)
        let renamed = rename(&posts[0], "renamed-post").unwrap();
        assert_eq!(
            renamed.file_name().unwrap().to_str().unwrap(),
            "renamed-post.md"
        );
        assert!(!path.exists());
        let posts = discover(&dir).unwrap();
        assert_eq!(posts.len(), 1);

        // rejects path separators
        assert!(rename(&posts[0], "../escape").is_err());

        // delete
        delete(&posts[0]).unwrap();
        assert!(discover(&dir).unwrap().is_empty());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn strips_toml_front_matter_for_body() {
        let body =
            strip_front_matter("+++\ntitle = \"x\"\ndate = 2020-01-01\n+++\n\n# Heading\n\ntext\n");
        assert!(body.starts_with("# Heading"));
        assert!(!body.contains("title ="));
    }

    #[test]
    fn body_without_front_matter_is_returned_verbatim() {
        let src = "# Just markdown\n\nno front matter here\n";
        assert_eq!(strip_front_matter(src), src);
    }

    #[test]
    fn set_draft_flips_existing_field() {
        let dir = scratch();
        let path = dir.join("p.md");
        fs::write(
            &path,
            "+++\ntitle = \"x\"\ndate = 2020-01-01\ndraft = true\n[taxonomies]\ntags = []\n+++\n\nbody\n",
        )
        .unwrap();

        set_draft(&path, false).unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert!(after.contains("draft = false"));
        assert!(!after.contains("draft = true"));
        // Body and other fields preserved.
        assert!(after.contains("title = \"x\""));
        assert!(after.contains("[taxonomies]"));
        assert!(after.ends_with("body\n"));
        assert!(!parse_post(&path).unwrap().draft);

        set_draft(&path, true).unwrap();
        assert!(parse_post(&path).unwrap().draft);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn set_date_changes_field_when_valid() {
        let dir = scratch();
        let path = dir.join("p.md");
        fs::write(
            &path,
            "+++\ntitle = \"x\"\ndate = 2020-01-01\ndraft = false\n+++\n\nbody\n",
        )
        .unwrap();

        set_date(&path, "2031-07-04").unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert!(after.contains("date = 2031-07-04"));
        assert!(!after.contains("2020-01-01"));
        assert_eq!(parse_post(&path).unwrap().date, "2031-07-04");
        // Other fields and body untouched.
        assert!(after.contains("draft = false"));
        assert!(after.ends_with("body\n"));

        // Invalid date is rejected and the file is left unchanged.
        let before = fs::read_to_string(&path).unwrap();
        assert!(set_date(&path, "2031-02-30").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), before);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn set_date_inserts_after_title_when_missing() {
        let dir = scratch();
        let path = dir.join("p.md");
        fs::write(&path, "+++\ntitle = \"x\"\n+++\n\nbody\n").unwrap();
        set_date(&path, "2025-05-05").unwrap();
        let after = fs::read_to_string(&path).unwrap();
        let title_pos = after.find("title =").unwrap();
        let date_pos = after.find("date = 2025-05-05").unwrap();
        assert!(title_pos < date_pos);
        assert_eq!(parse_post(&path).unwrap().date, "2025-05-05");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn set_draft_inserts_missing_field_before_tables() {
        let dir = scratch();
        let path = dir.join("p.md");
        fs::write(
            &path,
            "+++\ntitle = \"x\"\ndate = 2020-01-01\n[taxonomies]\ntags = []\n+++\n\nbody\n",
        )
        .unwrap();

        set_draft(&path, true).unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert!(after.contains("draft = true"));
        // Inserted inside the top-level block, before [taxonomies].
        let draft_pos = after.find("draft = true").unwrap();
        let tax_pos = after.find("[taxonomies]").unwrap();
        assert!(draft_pos < tax_pos);
        assert!(parse_post(&path).unwrap().draft);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn index_and_non_markdown_are_ignored() {
        let dir = scratch();
        fs::write(dir.join("_index.md"), "+++\n+++\n").unwrap();
        fs::write(dir.join("data.json"), "{}").unwrap();
        create(&dir, "Real Post").unwrap();
        let posts = discover(&dir).unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].title, "Real Post");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn append_to_body_keeps_everything_before() {
        let dir = scratch();
        let path = dir.join("p.md");
        let front = "+++\ntitle = \"x\"\n[taxonomies]\ntags = []\n+++\n";
        fs::write(&path, format!("{front}\nnotes\n\n\n")).unwrap();
        append_to_body(&path, "\n## Plan\n\n- a\n").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("{front}\nnotes\n\n## Plan\n\n- a\n")
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn replace_body_keeps_front_matter_verbatim() {
        let dir = scratch();
        let path = dir.join("p.md");
        let front = "+++\ntitle   =   \"x\"\ndraft = true\n+++\n";
        fs::write(&path, format!("{front}\nold body\n<!-- more -->\nmore\n")).unwrap();
        replace_body(&path, "\nNew intro.\n\n<!-- more -->\n\n## Part\n\n").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("{front}\nNew intro.\n\n<!-- more -->\n\n## Part\n")
        );
        assert_eq!(parse_post(&path).unwrap().title, "x");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn replace_body_without_front_matter_writes_the_body() {
        let dir = scratch();
        let path = dir.join("p.md");
        fs::write(&path, "just text\n").unwrap();
        replace_body(&path, "new").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
        // a front matter without its closing line is not one
        fs::write(&path, "+++\ntitle = \"x\"\n").unwrap();
        assert_eq!(front_matter_len("+++\ntitle = \"x\"\n"), 0);
        fs::remove_dir_all(&dir).unwrap();
    }
}

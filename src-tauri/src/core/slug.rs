use std::path::Path;

pub fn slugify(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut prev_dash = true;
    for ch in title.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("course");
    }
    out
}

pub fn unique_slug_in(root: &Path, title: &str) -> String {
    let base = slugify(title);
    if !root.join(&base).exists() {
        return base;
    }
    let mut n: u32 = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if !root.join(&candidate).exists() {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_lowercases_and_hyphenates_words() {
        assert_eq!(slugify("Hello World"), "hello-world");
    }

    #[test]
    fn slugify_strips_punctuation_and_collapses_separators() {
        assert_eq!(slugify("Intro to Rust: Part 1!"), "intro-to-rust-part-1");
        assert_eq!(slugify("  spaced   out  "), "spaced-out");
        assert_eq!(slugify("foo/bar_baz"), "foo-bar-baz");
    }

    #[test]
    fn slugify_falls_back_when_no_alphanumerics() {
        assert_eq!(slugify(""), "course");
        assert_eq!(slugify("   "), "course");
        assert_eq!(slugify("!!!"), "course");
    }

    #[test]
    fn unique_slug_in_returns_base_when_no_collision() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(unique_slug_in(dir.path(), "Hello World"), "hello-world");
    }

    #[test]
    fn unique_slug_in_appends_numeric_suffix_on_collision() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("hello-world")).unwrap();
        assert_eq!(unique_slug_in(dir.path(), "Hello World"), "hello-world-2");

        std::fs::create_dir(dir.path().join("hello-world-2")).unwrap();
        assert_eq!(unique_slug_in(dir.path(), "Hello World"), "hello-world-3");
    }
}

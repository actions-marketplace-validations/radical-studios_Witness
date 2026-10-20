//! Parser for `git diff -U0` output.

use witness_core::Hunk;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DiffParseError {
    #[error("line {line}: malformed hunk header {header:?}")]
    BadHunkHeader { line: usize, header: String },
    #[error("line {line}: hunk before any file header")]
    HunkWithoutFile { line: usize },
}

#[derive(Default)]
struct FileState {
    old_path: Option<String>,
    new_path: Option<String>,
    /// Path from the `diff --git` line, used when there are no `---`/`+++`
    /// lines (binary files).
    header_path: Option<String>,
}

impl FileState {
    /// Path at head; for deleted files, the old path.
    fn path(&self) -> Option<String> {
        self.new_path
            .clone()
            .or_else(|| self.old_path.clone())
            .or_else(|| self.header_path.clone())
    }
}

/// Parses unified diff text (as produced with `--src-prefix=a/
/// --dst-prefix=b/`) into hunks. Hunk lines are consumed by count, so added
/// lines that themselves start with `+++` or `diff` are handled.
pub fn parse_unified_diff(text: &str) -> Result<Vec<Hunk>, DiffParseError> {
    let mut hunks = Vec::new();
    let mut file: Option<FileState> = None;
    let mut current: Option<(Hunk, u32, u32)> = None; // hunk, old left, new left

    for (idx, line) in text.lines().enumerate() {
        let line_no = idx + 1;
        if let Some((hunk, old_left, new_left)) = current.as_mut() {
            if *old_left > 0 || *new_left > 0 {
                match line.as_bytes().first() {
                    Some(b'+') => {
                        hunk.added.push(line[1..].to_owned());
                        *new_left = new_left.saturating_sub(1);
                        continue;
                    }
                    Some(b'-') => {
                        hunk.removed.push(line[1..].to_owned());
                        *old_left = old_left.saturating_sub(1);
                        continue;
                    }
                    Some(b' ') => {
                        *old_left = old_left.saturating_sub(1);
                        *new_left = new_left.saturating_sub(1);
                        continue;
                    }
                    Some(b'\\') => continue,
                    _ => {}
                }
            } else if line.starts_with('\\') {
                continue;
            }
            if let Some((done, _, _)) = current.take() {
                hunks.push(done);
            }
        }

        if let Some(rest) = line.strip_prefix("diff --git ") {
            file = Some(FileState {
                header_path: header_new_path(rest),
                ..FileState::default()
            });
        } else if let Some(rest) = line.strip_prefix("--- ") {
            if let Some(f) = file.as_mut() {
                f.old_path = strip_prefix_path(rest, "a/");
            }
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            if let Some(f) = file.as_mut() {
                f.new_path = strip_prefix_path(rest, "b/");
            }
        } else if line.starts_with("@@") {
            let path = file
                .as_ref()
                .and_then(FileState::path)
                .ok_or(DiffParseError::HunkWithoutFile { line: line_no })?;
            let (old_start, old_count, new_start, new_count) =
                parse_hunk_header(line).ok_or_else(|| DiffParseError::BadHunkHeader {
                    line: line_no,
                    header: line.to_owned(),
                })?;
            current = Some((
                Hunk {
                    file: path,
                    old_start,
                    old_count,
                    new_start,
                    new_count,
                    added: Vec::new(),
                    removed: Vec::new(),
                    binary: false,
                },
                old_count,
                new_count,
            ));
        } else if let Some(rest) = line.strip_prefix("Binary files ") {
            let path = binary_new_path(rest)
                .or_else(|| file.as_ref().and_then(FileState::path))
                .ok_or(DiffParseError::HunkWithoutFile { line: line_no })?;
            hunks.push(Hunk {
                file: path,
                old_start: 0,
                old_count: 0,
                new_start: 0,
                new_count: 0,
                added: Vec::new(),
                removed: Vec::new(),
                binary: true,
            });
        }
    }
    if let Some((done, _, _)) = current {
        hunks.push(done);
    }
    Ok(hunks)
}

/// `@@ -a[,b] +c[,d] @@ ...`
fn parse_hunk_header(line: &str) -> Option<(u32, u32, u32, u32)> {
    let mut parts = line.split(' ');
    if parts.next()? != "@@" {
        return None;
    }
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    if parts.next()? != "@@" {
        return None;
    }
    let range = |s: &str| -> Option<(u32, u32)> {
        match s.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (os, oc) = range(old)?;
    let (ns, nc) = range(new)?;
    Some((os, oc, ns, nc))
}

/// Path from a `---`/`+++` line; `None` for `/dev/null`.
fn strip_prefix_path(raw: &str, prefix: &str) -> Option<String> {
    let raw = raw.trim_end_matches('\t');
    let path = unquote(raw);
    if path == "/dev/null" {
        return None;
    }
    Some(path.strip_prefix(prefix).unwrap_or(&path).to_owned())
}

/// New path from `a/x b/y` (ambiguous with spaces; best effort).
fn header_new_path(rest: &str) -> Option<String> {
    if rest.starts_with('"') {
        let (_, new) = split_quoted_pair(rest)?;
        return Some(new.strip_prefix("b/").unwrap_or(&new).to_owned());
    }
    let idx = rest.rfind(" b/")?;
    Some(rest[idx + 3..].to_owned())
}

/// From `a/x and b/y differ`, the head path (or the old one if deleted).
fn binary_new_path(rest: &str) -> Option<String> {
    let rest = rest.strip_suffix(" differ")?;
    let (old, new) = rest.rsplit_once(" and ")?;
    strip_prefix_path(new, "b/").or_else(|| strip_prefix_path(old, "a/"))
}

fn split_quoted_pair(rest: &str) -> Option<(String, String)> {
    let end = closing_quote(rest)?;
    let first = unquote(&rest[..=end]);
    let second = unquote(rest[end + 1..].trim_start());
    Some((first, second))
}

fn closing_quote(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// Undoes git's C-style quoting of unusual paths.
fn unquote(s: &str) -> String {
    let Some(inner) = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return s.to_owned();
    };
    let mut bytes = Vec::with_capacity(inner.len());
    let raw = inner.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'\\' && i + 1 < raw.len() {
            let c = raw[i + 1];
            let octal = raw
                .get(i + 1..i + 4)
                .filter(|d| d.iter().all(|b| (b'0'..=b'7').contains(b)))
                .map(|d| {
                    d.iter()
                        .fold(0u8, |acc, b| acc.wrapping_mul(8).wrapping_add(b - b'0'))
                });
            if let Some(v) = octal {
                bytes.push(v);
                i += 4;
                continue;
            }
            bytes.push(match c {
                b'n' => b'\n',
                b't' => b'\t',
                b'r' => b'\r',
                other => other,
            });
            i += 2;
        } else {
            bytes.push(raw[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A hunk's header numbers and its removed and added lines.
    type GenHunk = (u32, Vec<String>, u32, Vec<String>);

    fn gen_hunk() -> impl Strategy<Value = GenHunk> {
        // Line text may itself look like diff syntax (`+++ x`, `diff --git`,
        // `@@ ...`): hunks are consumed by count, so that must not matter.
        let line = prop_oneof![
            "[^\n\r]{0,20}",
            Just("+++ b/evil".to_owned()),
            Just("diff --git a/x b/x".to_owned()),
            Just("@@ -1 +1 @@".to_owned()),
        ];
        (
            1u32..500,
            proptest::collection::vec(line.clone(), 0..4),
            1u32..500,
            proptest::collection::vec(line, 0..4),
        )
            .prop_filter("a hunk changes something", |(_, r, _, a)| {
                !r.is_empty() || !a.is_empty()
            })
    }

    proptest! {
        #[test]
        fn parses_generated_hunks_exactly(hunks in proptest::collection::vec(gen_hunk(), 1..5)) {
            let mut text = String::from("diff --git a/f.rs b/f.rs\nindex 1..2 100644\n--- a/f.rs\n+++ b/f.rs\n");
            for (old_start, removed, new_start, added) in &hunks {
                text.push_str(&format!(
                    "@@ -{old_start},{} +{new_start},{} @@ fn x()\n",
                    removed.len(),
                    added.len()
                ));
                for l in removed {
                    text.push_str(&format!("-{l}\n"));
                }
                for l in added {
                    text.push_str(&format!("+{l}\n"));
                }
            }
            let parsed = parse_unified_diff(&text).unwrap();
            prop_assert_eq!(parsed.len(), hunks.len());
            for (got, (old_start, removed, new_start, added)) in parsed.iter().zip(&hunks) {
                prop_assert_eq!(got.file.as_str(), "f.rs");
                prop_assert_eq!(got.old_start, *old_start);
                prop_assert_eq!(got.old_count as usize, removed.len());
                prop_assert_eq!(got.new_start, *new_start);
                prop_assert_eq!(got.new_count as usize, added.len());
                prop_assert_eq!(&got.added, added);
                prop_assert_eq!(&got.removed, removed);
            }
        }

        #[test]
        fn never_panics_on_arbitrary_input(body in "\\PC{0,200}") {
            let _ = parse_unified_diff(&body);
            let _ = parse_unified_diff(&format!("diff --git a/x b/x\n--- a/x\n+++ b/x\n{body}"));
        }
    }

    fn summary(hunks: &[Hunk]) -> Vec<(String, u32, u32, u32, u32, usize, bool)> {
        hunks
            .iter()
            .map(|h| {
                (
                    h.file.clone(),
                    h.old_start,
                    h.old_count,
                    h.new_start,
                    h.new_count,
                    h.added.len(),
                    h.binary,
                )
            })
            .collect()
    }

    const DIFF: &str = r#"diff --git a/src/lib.rs b/src/lib.rs
index 3b18e51..a9c4f12 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -3 +3,2 @@ fn main() {
-    println!("hi");
+    let name = "world";
+    println!("hi {name}");
@@ -10,2 +11,0 @@ fn other() {
-    a();
-    b();
@@ -20,0 +20 @@ fn last() {
++++ not a header
diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..e69de29
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+hello
+\ No newline at end of file
\ No newline at end of file
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index e69de29..0000000
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/old name.rs b/new name.rs
similarity 90%
rename from old name.rs
rename to new name.rs
index 1..2 100644
--- a/old name.rs
+++ b/new name.rs
@@ -5 +5 @@
-x
+y
diff --git a/logo.png b/logo.png
index 1..2 100644
Binary files a/logo.png and b/logo.png differ
diff --git "a/tab\there.txt" "b/tab\there.txt"
index 1..2 100644
--- "a/tab\there.txt"
+++ "b/tab\there.txt"
@@ -1 +1 @@
-a
+b
"#;

    #[test]
    fn parses_hunks_of_every_shape() {
        let hunks = parse_unified_diff(DIFF).unwrap();
        assert_eq!(
            summary(&hunks),
            vec![
                ("src/lib.rs".into(), 3, 1, 3, 2, 2, false),
                ("src/lib.rs".into(), 10, 2, 11, 0, 0, false),
                ("src/lib.rs".into(), 20, 0, 20, 1, 1, false),
                ("new.txt".into(), 0, 0, 1, 2, 2, false),
                ("gone.txt".into(), 1, 1, 0, 0, 0, false),
                ("new name.rs".into(), 5, 1, 5, 1, 1, false),
                ("logo.png".into(), 0, 0, 0, 0, 0, true),
                ("tab\there.txt".into(), 1, 1, 1, 1, 1, false),
            ]
        );
        assert_eq!(
            hunks[0].added,
            vec!["    let name = \"world\";", "    println!(\"hi {name}\");"]
        );
        assert_eq!(hunks[2].added, vec!["+++ not a header"]);
    }

    #[test]
    fn empty_diff_has_no_hunks() {
        assert!(parse_unified_diff("").unwrap().is_empty());
    }

    #[test]
    fn rejects_malformed_header() {
        let text = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,a +1 @@\n";
        assert!(matches!(
            parse_unified_diff(text),
            Err(DiffParseError::BadHunkHeader { line: 4, .. })
        ));
        assert_eq!(
            parse_unified_diff("@@ -1 +1 @@\n"),
            Err(DiffParseError::HunkWithoutFile { line: 1 })
        );
    }

    #[test]
    fn unquotes_octal_escapes() {
        assert_eq!(unquote(r#""caf\303\251.txt""#), "café.txt");
        assert_eq!(unquote("plain"), "plain");
    }
}

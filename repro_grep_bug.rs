use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::collections::VecDeque;

const MAX_READ_BYTES: usize = 512 * 1024;
const DEFAULT_LIST_LIMIT: usize = 200;
const HARD_LIST_LIMIT: usize = 1000;
const DEFAULT_SEARCH_LIMIT: usize = 100;
const HARD_SEARCH_LIMIT: usize = 500;
const HARD_SEARCH_CONTEXT_LINES: usize = 20;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchTextEntry {
    pub path: String,
    pub line: usize,
    pub text: String,
    pub is_context: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchTextOutput {
    pub pattern: String,
    pub path: String,
    pub backend: String,
    pub backend_note: String,
    pub match_count: usize,
    pub truncated: bool,
    pub limit: usize,
    pub results: Vec<SearchTextEntry>,
}

#[derive(Clone, Copy)]
struct ResolvedSearchTextOptions<'a> {
    pattern: &'a str,
    glob: Option<&'a str>,
    fixed_strings: bool,
    case_insensitive: bool,
    before: usize,
    after: usize,
    max_matches: usize,
    max_matches_per_file: Option<usize>,
    include_hidden: bool,
    no_ignore: bool,
}

enum SearchBackendError {
    Unavailable,
    Failed(String),
}

fn tool_path_string(path: &Path) -> String {
    let path = path.display().to_string();
    #[cfg(windows)]
    {
        path.replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        path
    }
}

fn to_workspace_relative(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".into(),
        Ok(rel) => tool_path_string(rel),
        Err(_) => tool_path_string(path),
    }
}

fn parse_grep_line_prefix(line: &str) -> Option<(usize, char, &str)> {
    let mut digits_end = 0_usize;
    for (idx, ch) in line.char_indices() {
        if ch.is_ascii_digit() {
            digits_end = idx + ch.len_utf8();
            continue;
        }
        if digits_end == 0 || (ch != ':' && ch != '-') {
            return None;
        }
        let line_number = line[..digits_end].parse::<usize>().ok()?;
        let text_start = idx + ch.len_utf8();
        return Some((line_number, ch, &line[text_start..]));
    }
    None
}

fn parse_grep_search_entry(root: &Path, file: &Path, line: &str) -> Option<SearchTextEntry> {
    let (line_number, separator, text) = parse_grep_line_prefix(line)?;
    Some(SearchTextEntry {
        path: to_workspace_relative(root, file),
        line: line_number,
        text: text.to_string(),
        is_context: separator == '-',
    })
}

fn search_text_grep(
    root: &Path,
    start: &Path,
    options: ResolvedSearchTextOptions<'_>,
) -> Result<SearchTextOutput, SearchBackendError> {
    // Simplified file collection for reproduction
    let mut files = Vec::new();
    if start.is_dir() {
        // Just hardcode what's in the test
        files.push(start.join("src/main.rs"));
        files.push(start.join("notes.txt"));
    } else {
        files.push(start.to_path_buf());
    }
    files.sort();

    // Filter by glob *.rs
    if let Some(glob) = options.glob {
        files.retain(|f| f.to_string_lossy().ends_with(".rs"));
    }

    let mut results = Vec::new();
    let mut returned_matches = 0_usize;
    let mut truncated = false;

    for file in files.iter() {
        if returned_matches >= options.max_matches {
            truncated = true;
            break;
        }
        let remaining_matches = options.max_matches - returned_matches;
        let file_match_limit = options
            .max_matches_per_file
            .map(|value| value.min(remaining_matches))
            .unwrap_or(remaining_matches);
        let mut command = ProcessCommand::new("grep");
        command
            .current_dir(root)
            .arg("-n")
            .arg("-I")
            .arg("-m")
            .arg(file_match_limit.to_string());
        command.arg("-E");
        command.arg("--").arg(options.pattern).arg(file);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());

        let output = command.output().map_err(|e| SearchBackendError::Failed(e.to_string()))?;

        let mut file_matches = 0_usize;
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if line == "--" {
                continue;
            }
            let Some(entry) = parse_grep_search_entry(root, file, line) else {
                continue;
            };
            if !entry.is_context {
                if returned_matches >= options.max_matches {
                    truncated = true;
                    break;
                }
                if let Some(max_per_file) = options.max_matches_per_file {
                    if file_matches >= max_per_file {
                        continue;
                    }
                }
                file_matches += 1;
                returned_matches += 1;
            }
            results.push(entry);
        }
    }

    Ok(SearchTextOutput {
        pattern: options.pattern.to_string(),
        path: to_workspace_relative(root, start),
        backend: "grep".into(),
        backend_note: "rg not found; used grep".into(),
        match_count: returned_matches,
        truncated,
        limit: options.max_matches,
        results,
    })
}

fn main() {
    let workspace_root = std::env::current_dir().unwrap().join("repro_workspace");
    let _ = fs::remove_dir_all(&workspace_root);
    fs::create_dir_all(workspace_root.join("src")).unwrap();
    fs::write(workspace_root.join("notes.txt"), "alpha1\n").unwrap();
    fs::write(workspace_root.join("src/main.rs"), "alpha1\nbeta\nalpha2\n").unwrap();

    let options = ResolvedSearchTextOptions {
        pattern: "alpha[0-9]",
        glob: Some("*.rs"),
        fixed_strings: false,
        case_insensitive: false,
        before: 0,
        after: 0,
        max_matches: 1,
        max_matches_per_file: None,
        include_hidden: false,
        no_ignore: false,
    };

    let output = search_text_grep(&workspace_root, &workspace_root, options).unwrap();
    println!("Backend: {}", output.backend);
    println!("Match count: {}", output.match_count);
    println!("Truncated: {}", output.truncated);

    if output.truncated {
        println!("SUCCESS: Reproduced truncation issue (it is true)");
    } else {
        println!("FAILURE: Reproduced truncation issue (it is false, but should be true)");
    }
}

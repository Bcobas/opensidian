// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::BTreeMap;

use pulldown_cmark::{Event, Options, Parser};
use serde::Serialize;
use yaml_rust2::{Yaml, YamlEmitter, YamlLoader};

#[derive(Serialize)]
pub struct Task {
    note: String,
    line: usize,
    text: String,
    checked: bool,
}

pub fn tasks<'a>(note: &'a str, content: &'a str) -> impl Iterator<Item = Task> + 'a {
    let (body, offset) = frontmatter(content).map_or((content, 0), |(_, body)| {
        (body, content.len() - body.len())
    });
    let starts: Vec<_> = std::iter::once(0).chain(content.match_indices('\n').map(|(i, _)| i + 1)).collect();
    Parser::new_ext(body, Options::ENABLE_TASKLISTS).into_offset_iter().filter_map(move |(event, range)| {
        let Event::TaskListMarker(checked) = event else { return None };
        let text = body[range.end..].lines().next().unwrap_or("").trim();
        Some(Task { note: note.to_owned(), line: starts.partition_point(|&n| n <= range.start + offset), text: text.to_owned(), checked })
    })
}

#[derive(Serialize)]
pub struct Row {
    name: String,
    values: BTreeMap<String, String>,
}

pub fn row(name: &str, content: &str) -> Row {
    let mut values = BTreeMap::new();
    if let Some((yaml, _)) = frontmatter(content).filter(|(yaml, _)| yaml.len() <= 128 * 1024) {
        if let Ok(docs) = YamlLoader::load_from_str(yaml) {
            if let Some(Yaml::Hash(map)) = docs.first() {
                for (key, value) in map {
                    if let Some(key) = key.as_str() {
                        values.insert(key.to_owned(), display(value));
                    }
                }
            }
        }
    }
    Row { name: name.to_owned(), values }
}

fn display(value: &Yaml) -> String {
    match value {
        Yaml::String(s) | Yaml::Real(s) => s.to_owned(),
        Yaml::Integer(n) => n.to_string(),
        Yaml::Boolean(b) => b.to_string(),
        Yaml::Null | Yaml::BadValue => String::new(),
        Yaml::Array(items) => items.iter().map(display).collect::<Vec<_>>().join(", "),
        other => {
            let mut out = String::new();
            let _ = YamlEmitter::new(&mut out).dump(other);
            out.strip_prefix("---\n").unwrap_or(&out).trim().to_owned()
        }
    }
}

fn frontmatter(content: &str) -> Option<(&str, &str)> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let (open, after) = content.split_once('\n')?;
    if open.trim_end() != "---" { return None; }
    let mut offset = 0;
    for line in after.split_inclusive('\n') {
        if matches!(line.trim_end(), "---" | "...") {
            return Some((&after[..offset], &after[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_real_tasks_and_yaml_properties() {
        let text = "\u{feff}---\r\nstatus: \"in progress\"\r\ntags:\r\n  - planning\r\n  - work\r\n---\r\n- [ ] open\r\n- [x] done\r\n```\r\n- [ ] example\r\n```\r\n";
        let properties = row("note", text);
        assert_eq!(properties.values["status"], "in progress");
        assert_eq!(properties.values["tags"], "planning, work");
        let tasks: Vec<_> = tasks("note", text).collect();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].line, 7);
        assert_eq!(tasks[0].text, "open");
        assert!(!tasks[0].checked);
        assert!(tasks[1].checked);
    }
}

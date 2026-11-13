// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::BTreeMap;

use pulldown_cmark::{Event, Options, Parser};
use serde::Serialize;
#[cfg(windows)]
use serde::Deserialize;
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
    let mut first_in_item = false;
    Parser::new_ext(body, Options::ENABLE_TASKLISTS).into_offset_iter().filter_map(move |(event, range)| {
        let first = first_in_item;
        first_in_item = matches!(event, Event::Start(pulldown_cmark::Tag::Item))
            || first && matches!(event, Event::Start(pulldown_cmark::Tag::Paragraph));
        let (checked, end) = match event {
            Event::TaskListMarker(checked) => (checked, range.end),
            Event::Text(_) if first => {
                let source = body[range.start..].lines().next()?;
                let (_, bytes) = custom_marker(source)?;
                (true, range.start + bytes)
            }
            _ => return None,
        };
        let text = body[end..].lines().next().unwrap_or("").trim();
        Some(Task { note: note.to_owned(), line: starts.partition_point(|&n| n <= range.start + offset), text: text.to_owned(), checked })
    })
}

pub fn custom_marker(text: &str) -> Option<(char, usize)> {
    let mut chars = text.char_indices();
    if chars.next()?.1 != '[' { return None; }
    let (_, status) = chars.next()?;
    let (close, bracket) = chars.next()?;
    if bracket != ']' || matches!(status, ' ' | 'x' | 'X' | '[' | ']') || status.is_whitespace() { return None; }
    let rest = &text[close + 1..];
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) { return None; }
    Some((status, text.len() - rest.trim_start().len()))
}

#[derive(Clone, Serialize)]
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

#[cfg(windows)]
#[derive(Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PropertyValue {
    Text(String),
    Number(String),
    Checkbox(bool),
    List(Vec<String>),
}

#[cfg(windows)]
pub fn add_property(content: &str, key: &str, value: PropertyValue) -> Result<String, String> {
    const MAX_YAML_BYTES: usize = 128 * 1024;
    let key = key.trim();
    if key.is_empty() || key == "<<" || key.len() > 256 || key.chars().any(char::is_control) {
        return Err("Enter a valid property name".into());
    }
    let eol = if content.split_once('\n').is_some_and(|(line, _)| line.ends_with('\r')) { "\r\n" } else { "\n" };
    let body = content.strip_prefix('\u{feff}').unwrap_or(content);
    let existing = frontmatter(content);
    if existing.is_none() && body.lines().next().is_some_and(|line| line.trim_end() == "---") {
        return Err("The YAML block has no closing delimiter".into());
    }
    let old_yaml = existing.map_or("", |(yaml, _)| yaml);
    if old_yaml.len() > MAX_YAML_BYTES {
        return Err("The YAML block is too large to edit".into());
    }
    let old = yaml_mapping(old_yaml)?;
    if old.keys().any(|entry| entry.as_str().is_some_and(|name| name.eq_ignore_ascii_case(key))) {
        return Err(format!("The property “{key}” already exists"));
    }

    // Plain keys for ordinary names keep properties such as tags readable by the tag index.
    let encoded_key = if key.starts_with(char::is_alphabetic) &&
        key.chars().all(|c| c.is_alphanumeric() || matches!(c, ' ' | '_' | '-')) &&
        matches!(Yaml::from_str(key), Yaml::String(_)) {
        key.to_owned()
    } else {
        serde_json::to_string(key).map_err(|error| error.to_string())?
    };
    let quote = |text: &str| serde_json::to_string(text).map_err(|error| error.to_string());
    let mut line = format!("{encoded_key}:");
    let expected = match value {
        PropertyValue::Text(text) => {
            line.push_str(&format!(" {}", quote(&text)?));
            Yaml::String(text)
        }
        PropertyValue::Number(text) => {
            let number = text.trim().parse::<serde_json::Number>().map_err(|_| "Enter a valid number")?;
            if number.is_u64() && number.as_i64().is_none() { return Err("The integer is too large".into()); }
            line.push_str(&format!(" {number}"));
            Yaml::from_str(&number.to_string())
        }
        PropertyValue::Checkbox(value) => {
            line.push_str(&format!(" {value}"));
            Yaml::Boolean(value)
        }
        PropertyValue::List(items) => {
            if items.is_empty() { line.push_str(" []"); }
            for item in &items { line.push_str(&format!("{eol}  - {}", quote(item)?)); }
            Yaml::Array(items.into_iter().map(Yaml::String).collect())
        }
    };
    line.push_str(eol);

    // Validate the splice without reserializing existing values, comments or anchors.
    let updated_yaml = format!("{old_yaml}{line}");
    if updated_yaml.len() > MAX_YAML_BYTES { return Err("The YAML block is too large to edit".into()); }
    let updated = yaml_mapping(&updated_yaml)?;
    if updated.len() != old.len() + 1 || updated.get(&Yaml::String(key.into())) != Some(&expected)
        || old.iter().any(|(key, value)| updated.get(key) != Some(value)) {
        return Err("This YAML layout cannot be edited safely. Use a block mapping.".into());
    }
    if let Some((yaml, _)) = existing {
        let at = content.len() - body.len() + body.find('\n').expect("validated opening delimiter") + 1 + yaml.len();
        Ok(format!("{}{line}{}", &content[..at], &content[at..]))
    } else {
        let bom = if body.len() == content.len() { "" } else { "\u{feff}" };
        let separator = if body.is_empty() || body.starts_with(eol) { "" } else { eol };
        Ok(format!("{bom}---{eol}{line}---{eol}{separator}{body}"))
    }
}

#[cfg(windows)]
fn yaml_mapping(text: &str) -> Result<yaml_rust2::yaml::Hash, String> {
    let mut docs = YamlLoader::load_from_str(text)
        .map_err(|error| format!("Could not edit the YAML block: {error}"))?;
    if docs.is_empty() { return Ok(Default::default()); }
    if docs.len() == 1 {
        if let Some(Yaml::Hash(map)) = docs.pop() { return Ok(map); }
    }
    Err("The YAML block must be a mapping of property names to values".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn adds_typed_properties_and_preserves_existing_yaml() {
        let original = "\u{feff}---\r\n# keep\r\npriority: 2\r\nnested:\r\n  status: ready\r\n---\r\n# Body\r\n";
        let added = add_property(original, "project: name", PropertyValue::Text("true\nvalue".into())).unwrap();
        assert_eq!(added, original.replace("---\r\n# Body", "\"project: name\": \"true\\nvalue\"\r\n---\r\n# Body"));
        let added = add_property(&added, "estimate", PropertyValue::Number("1.5".into())).unwrap();
        let added = add_property(&added, "done", PropertyValue::Checkbox(false)).unwrap();
        let added = add_property(&added, "tags", PropertyValue::List(vec!["work".into(), "review".into()])).unwrap();
        let properties = row("A", &added);
        assert_eq!(properties.values["project: name"], "true\nvalue");
        assert_eq!(properties.values["estimate"], "1.5");
        assert_eq!(properties.values["done"], "false");
        assert_eq!(properties.values["tags"], "work, review");
        assert_eq!(crate::index::tags_in(&added), ["review", "work"]);
        assert!(added.ends_with("# Body\r\n"));
    }

    #[cfg(windows)]
    #[test]
    fn creates_frontmatter_and_refuses_duplicate_or_invalid_input() {
        let original = "# Body\n";
        let added = add_property(original, "status", PropertyValue::Text("yes".into())).unwrap();
        assert_eq!(added, "---\nstatus: \"yes\"\n---\n\n# Body\n");
        assert!(add_property(&added, "STATUS", PropertyValue::Checkbox(true)).is_err());
        for invalid in ["---\nfoo: [\n---\n", "---\nfoo: value", "---\n{foo: bar}\n---\n", "---\n- item\n---\n"] {
            assert!(add_property(invalid, "new", PropertyValue::Checkbox(true)).is_err(), "{invalid}");
        }
        assert!(add_property(original, "number", PropertyValue::Number("NaN".into())).is_err());
    }

    #[test]
    fn reads_real_tasks_and_yaml_properties() {
        let text = "\u{feff}---\r\nstatus: \"in progress\"\r\ntags:\r\n  - planning\r\n  - work\r\n---\r\n- [ ] open\r\n- [x] done\r\n- [/] partial\r\n```\r\n- [ ] example\r\n```\r\n";
        let properties = row("note", text);
        assert_eq!(properties.values["status"], "in progress");
        assert_eq!(properties.values["tags"], "planning, work");
        let tasks: Vec<_> = tasks("note", text).collect();
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].line, 7);
        assert_eq!(tasks[0].text, "open");
        assert!(!tasks[0].checked);
        assert!(tasks[1].checked);
        assert_eq!(tasks[2].text, "partial");
    }
}

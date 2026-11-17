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
    row_from_properties(name, &properties(content))
}

pub fn properties(content: &str) -> yaml_rust2::yaml::Hash {
    if let Some((yaml, _)) = frontmatter(content).filter(|(yaml, _)| yaml.len() <= 128 * 1024) {
        if let Ok(docs) = YamlLoader::load_from_str(yaml) {
            if let Some(Yaml::Hash(map)) = docs.into_iter().next() {
                return map;
            }
        }
    }
    Default::default()
}

pub fn row_from_properties(name: &str, properties: &yaml_rust2::yaml::Hash) -> Row {
    let values = properties.iter().filter_map(|(key, value)| {
        Some((key.as_str()?.to_owned(), display(value)))
    }).collect();
    Row { name: name.to_owned(), values }
}

pub(crate) fn display(value: &Yaml) -> String {
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
#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PropertyValue {
    Text(String),
    Number(String),
    Checkbox(bool),
    List(Vec<String>),
}

#[cfg(windows)]
const MAX_YAML_BYTES: usize = 128 * 1024;

#[cfg(windows)]
fn checked_yaml(content: &str) -> Result<Option<(&str, &str)>, String> {
    let existing = frontmatter(content);
    let body = content.strip_prefix('\u{feff}').unwrap_or(content);
    if existing.is_none() && body.lines().next().is_some_and(|line| line.trim_end() == "---") {
        return Err("The YAML block has no closing delimiter".into());
    }
    if existing.is_some_and(|(yaml, _)| yaml.len() > MAX_YAML_BYTES) {
        return Err("The YAML block is too large to edit".into());
    }
    Ok(existing)
}

#[cfg(windows)]
pub fn add_property(content: &str, key: &str, value: PropertyValue) -> Result<String, String> {
    let key = key.trim();
    if key.is_empty() || key == "<<" || key.len() > 256 || key.chars().any(char::is_control) {
        return Err("Enter a valid property name".into());
    }
    let eol = if content.split_once('\n').is_some_and(|(line, _)| line.ends_with('\r')) { "\r\n" } else { "\n" };
    let body = content.strip_prefix('\u{feff}').unwrap_or(content);
    let existing = checked_yaml(content)?;
    let old_yaml = existing.map_or("", |(yaml, _)| yaml);
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
    let (inline, items, expected) = encode_property(value, eol)?;
    let line = format!("{encoded_key}:{inline}{eol}{items}");

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

#[cfg(windows)]
fn encode_property(value: PropertyValue, eol: &str) -> Result<(String, String, Yaml), String> {
    let quote = |text: &str| serde_json::to_string(text).map_err(|error| error.to_string());
    let mut items = String::new();
    let (inline, expected) = match value {
        PropertyValue::Text(text) => (format!(" {}", quote(&text)?), Yaml::String(text)),
        PropertyValue::Number(text) => {
            let number = text.trim().parse::<serde_json::Number>().map_err(|_| "Enter a valid number")?;
            if number.is_u64() && number.as_i64().is_none() { return Err("The integer is too large".into()); }
            (format!(" {number}"), Yaml::from_str(&number.to_string()))
        }
        PropertyValue::Checkbox(value) => (format!(" {value}"), Yaml::Boolean(value)),
        PropertyValue::List(values) => {
            for value in &values { items.push_str(&format!("  - {}{eol}", quote(value)?)); }
            let inline = if values.is_empty() { " []" } else { "" };
            (inline.into(), Yaml::Array(values.into_iter().map(Yaml::String).collect()))
        }
    };
    Ok((inline, items, expected))
}

#[cfg(windows)]
#[derive(Serialize)]
pub struct PropertyField {
    key: String,
    value: Option<PropertyValue>,
    display: String,
}

#[cfg(windows)]
pub fn property_fields(content: &str) -> Result<Vec<PropertyField>, String> {
    let Some((yaml, _)) = checked_yaml(content)? else { return Ok(Vec::new()); };
    let map = yaml_mapping(yaml)?;
    let slots = property_slots(yaml, &map);
    Ok(map.iter().filter_map(|(key, value)| {
        let key = key.as_str()?;
        Some(PropertyField {
            key: key.to_owned(),
            value: slots.contains_key(key).then(|| editable_value(value)).flatten(),
            display: display(value),
        })
    }).collect())
}

#[cfg(windows)]
fn editable_value(value: &Yaml) -> Option<PropertyValue> {
    Some(match value {
        Yaml::String(text) => PropertyValue::Text(text.clone()),
        Yaml::Integer(number) => PropertyValue::Number(number.to_string()),
        Yaml::Real(number) => {
            // Retain the original decimal when JSON accepts it, including its precision.
            let text = if number.parse::<serde_json::Number>().is_ok() { number.clone() }
                else { value.as_f64().filter(|n| n.is_finite())?.to_string() };
            PropertyValue::Number(text)
        }
        Yaml::Boolean(checked) => PropertyValue::Checkbox(*checked),
        Yaml::Array(items) => PropertyValue::List(items.iter().map(|item| {
            item.as_str().map(str::to_owned)
        }).collect::<Option<_>>()?),
        _ => return None,
    })
}

#[cfg(windows)]
struct PropertySlot<'a> {
    start: usize,
    end: usize,
    suffix: &'a str,
    eol: &'a str,
}

#[cfg(windows)]
fn property_slots<'a>(yaml: &'a str, map: &yaml_rust2::yaml::Hash) -> BTreeMap<String, PropertySlot<'a>> {
    use yaml_rust2::scanner::{Scanner, TScalarStyle, Token, TokenType::*};

    let tokens: Vec<_> = Scanner::new(yaml.chars()).collect();
    if !matches!(tokens.get(1), Some(Token(_, BlockMappingStart))) { return BTreeMap::new(); }
    let offsets: Vec<_> = yaml.char_indices().map(|(byte, _)| byte).chain([yaml.len()]).collect();
    let mut slots = BTreeMap::new();
    for (index, pair) in tokens.windows(4).enumerate() {
        let [Token(mark, Key), Token(key_mark, Scalar(_, key)), Token(colon, Value), Token(_, first)] = pair else { continue; };
        if mark.col() != 0 || mark != key_mark || mark.line() != colon.line() { continue; }
        if key == "<<" { continue; }
        let Some(value) = map.get(&Yaml::String(key.clone())) else { continue; };
        if !matches!(first, Scalar(TScalarStyle::Plain | TScalarStyle::SingleQuoted | TScalarStyle::DoubleQuoted, _)
            | FlowSequenceStart | BlockSequenceStart | BlockEntry) { continue; }
        let start = offsets[colon.index()] + 1;
        if let Some(slot) = property_slot(yaml, start, value) {
            let decorated = tokens[index + 3..].iter().take_while(|Token(mark, _)| offsets[mark.index()] < slot.end)
                .any(|Token(_, token)| matches!(token, Anchor(_) | Alias(_) | Tag(..)));
            if !decorated { slots.insert(key.clone(), slot); }
        }
    }
    slots
}

#[cfg(windows)]
fn property_slot<'a>(yaml: &'a str, start: usize, value: &Yaml) -> Option<PropertySlot<'a>> {
    let (line, tail) = yaml[start..].split_once('\n')?;
    let (line, eol) = line.strip_suffix('\r').map_or((line, "\n"), |line| (line, "\r\n"));
    let parses_as_value = |source: &str| {
        yaml_mapping(&format!("entry:{source}")).ok().is_some_and(|map| {
            map.len() == 1 && map.get(&Yaml::String("entry".into())) == Some(value)
        })
    };
    let end_of_line = start + line.len() + eol.len();
    let mut end = end_of_line;
    let suffix;
    if matches!(value, Yaml::Array(_)) && (line.trim().is_empty() || line.trim_start().starts_with('#')) {
        for item in tail.split_inclusive('\n').take_while(|line| line.trim_start().starts_with("- ")) {
            // Comments attached to a particular list entry would lose their position.
            if item.contains('#') { return None; }
            end += item.len();
        }
        if end == end_of_line || !parses_as_value(&yaml[start..end]) { return None; }
        suffix = line;
    } else {
        if !parses_as_value(line) { return None; }
        let mut boundary = line.len();
        for (index, _) in line.match_indices('#').filter(|(index, _)| line[..*index].ends_with([' ', '\t'])).take(32) {
            if parses_as_value(&line[..index]) {
                boundary = index;
                break;
            }
        }
        if boundary == line.len() && line.match_indices('#').count() > 32 { return None; }
        suffix = &line[line[..boundary].trim_end_matches([' ', '\t']).len()..];
    }
    Some(PropertySlot { start, end, suffix, eol })
}

#[cfg(windows)]
pub fn set_property(content: &str, key: &str, value: PropertyValue) -> Result<String, String> {
    let (yaml, _) = checked_yaml(content)?.ok_or("This note has no properties")?;
    let expected = yaml_mapping(yaml)?;
    let yaml_key = Yaml::String(key.into());
    let old = expected.get(&yaml_key).and_then(editable_value).ok_or("This property cannot be edited here")?;
    if std::mem::discriminant(&old) != std::mem::discriminant(&value) {
        return Err("Keep the current property type".into());
    }
    let slots = property_slots(yaml, &expected);
    let slot = slots.get(key).ok_or("This YAML layout must be edited in Source mode")?;
    let (inline, items, updated) = encode_property(value, slot.eol)?;
    if expected.get(&yaml_key) == Some(&updated) { return Ok(content.to_owned()); }
    let suffix = if inline.is_empty() && slot.suffix.starts_with('#') { format!(" {}", slot.suffix) }
        else { slot.suffix.to_owned() };
    let replacement = format!("{inline}{suffix}{}{items}", slot.eol);
    let next = format!("{}{replacement}{}", &yaml[..slot.start], &yaml[slot.end..]);
    if next.len() > MAX_YAML_BYTES { return Err("The YAML block is too large to edit".into()); }
    let parsed = yaml_mapping(&next)?;
    if parsed.len() != expected.len() || expected.iter().any(|(key, old)| {
        let value = if key == &yaml_key { &updated } else { old };
        parsed.get(key) != Some(value)
    }) { return Err("This YAML layout must be edited in Source mode".into()); }
    let start = content.find('\n').expect("validated opening delimiter") + 1;
    Ok(format!("{}{next}{}", &content[..start], &content[start + yaml.len()..]))
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

    #[cfg(windows)]
    #[test]
    fn edits_typed_properties_without_rewriting_the_note() {
        let original = "\u{feff}---\r\n# keep\r\n\"estádo: actual\": 'en # curso'  # nota # final\r\npriority: 2\r\ndone: false\r\ntags: [planning, work] # categories\r\n---\r\n# Body\r\n";
        let fields = property_fields(original).unwrap();
        assert_eq!(fields.len(), 4);
        assert!(fields.iter().all(|field| field.value.is_some()));
        let next = set_property(original, "estádo: actual", PropertyValue::Text("hecho".into())).unwrap();
        assert_eq!(next, original.replace("'en # curso'", "\"hecho\""));
        let next = set_property(&next, "priority", PropertyValue::Number("3.5".into())).unwrap();
        let next = set_property(&next, "done", PropertyValue::Checkbox(true)).unwrap();
        let next = set_property(&next, "tags", PropertyValue::List(vec!["review".into(), "work".into()])).unwrap();
        assert!(next.contains("tags: # categories\r\n  - \"review\"\r\n  - \"work\"\r\n"));
        assert_eq!(crate::index::tags_in(&next), ["review", "work"]);
        assert_eq!(row("A", &next).values["priority"], "3.5");
        assert_eq!(row("A", &next).values["done"], "true");
        let next = set_property(&next, "tags", PropertyValue::List(vec![])).unwrap();
        assert!(next.ends_with("tags: [] # categories\r\n---\r\n# Body\r\n"));
        assert!(set_property(&next, "done", PropertyValue::Text("no".into())).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn keeps_ambiguous_or_complex_properties_read_only() {
        let duplicate = "---\nstatus: old\nstatus: active\n---\n";
        assert!(property_fields(duplicate).is_err());
        let original = "---\nshared: &label ready\ncopy: *label\nlong: |\n  multi\n  line\nitems:\n  - example # keep\nmap: {x: 2}\n---\n";
        let fields = property_fields(original).unwrap();
        assert_eq!(fields.len(), 5);
        for field in fields {
            assert!(field.value.is_none(), "{}", field.key);
            assert!(set_property(original, &field.key, PropertyValue::Text("replacement".into())).is_err());
        }
        let indentless = "---\nitems:\n- first\n- second\n# keep\nstatus: active\n---\n";
        let next = set_property(indentless, "items", PropertyValue::List(vec!["new".into()])).unwrap();
        assert_eq!(next, "---\nitems:\n  - \"new\"\n# keep\nstatus: active\n---\n");
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

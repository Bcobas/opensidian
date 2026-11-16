// SPDX-License-Identifier: GPL-3.0-or-later

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use yaml_rust2::{Yaml, YamlEmitter, YamlLoader};

use crate::{note_views, safe_rel, vaultfs::Vault, Index};

const MAX_BYTES: u64 = 128 * 1024;
const MAX_CSV_BYTES: usize = 32 * 1024 * 1024;
const MAX_COLUMNS: usize = 128;
const FILE_NAME: &str = "file.name";

#[derive(Deserialize)]
pub struct NewBase {
    pub columns: Vec<String>,
    pub sort: String,
    pub descending: bool,
    pub folder: Option<String>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    pub group_by: Option<PropertyOrder>,
}

#[derive(Deserialize, Serialize, PartialEq)]
pub struct PropertyOrder {
    property: String,
    direction: Direction,
}

#[derive(Deserialize, Serialize, PartialEq)]
enum Direction {
    #[serde(rename = "ASC")]
    Ascending,
    #[serde(rename = "DESC")]
    Descending,
}

#[derive(Deserialize)]
pub struct Condition {
    property: String,
    operator: Operator,
    kind: ValueKind,
    value: String,
}

#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Operator { Eq, Ne, Ge, Le, Gt, Lt }

impl Operator {
    const ALL: [(Self, &'static str); 6] = [
        (Self::Eq, "=="), (Self::Ne, "!="), (Self::Ge, ">="),
        (Self::Le, "<="), (Self::Gt, ">"), (Self::Lt, "<"),
    ];
    fn symbol(self) -> &'static str {
        Self::ALL.iter().find(|(operator, _)| *operator == self).unwrap().1
    }
    fn test(self, order: Option<Ordering>) -> bool {
        match self {
            Self::Eq => order == Some(Ordering::Equal),
            Self::Ne => order != Some(Ordering::Equal),
            Self::Ge => matches!(order, Some(Ordering::Equal | Ordering::Greater)),
            Self::Le => matches!(order, Some(Ordering::Equal | Ordering::Less)),
            Self::Gt => order == Some(Ordering::Greater),
            Self::Lt => order == Some(Ordering::Less),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ValueKind { Text, Number, Checkbox }

impl Condition {
    fn expression(&self) -> Result<String, String> {
        let property = self.property.as_str();
        if property.trim().is_empty() || property.len() > 256 || property.chars().any(char::is_control) {
            return Err("Enter a valid property name".into());
        }
        let key = serde_json::to_string(property).map_err(|error| error.to_string())?;
        let literal = match self.kind {
            ValueKind::Text => serde_json::to_string(&self.value).map_err(|error| error.to_string())?,
            ValueKind::Number => self.value.trim().parse::<serde_json::Number>()
                .map_err(|_| "Enter a valid number")?.to_string(),
            ValueKind::Checkbox => self.value.parse::<bool>().map_err(|_| "Select true or false")?.to_string(),
        };
        let operator = self.operator.symbol();
        let expression = format!("note[{key}] {operator} {literal}");
        Filter::parse(&Yaml::String(expression.clone()), 0)?;
        Ok(expression)
    }
}

#[derive(Serialize)]
pub struct Base {
    revision: String,
    properties: BTreeSet<String>,
    views: Vec<Table>,
}

#[derive(Serialize)]
pub struct Table {
    id: usize,
    title: String,
    columns: Vec<String>,
    sort: Vec<PropertyOrder>,
    limit: Option<usize>,
    labels: BTreeMap<String, String>,
    rows: Vec<note_views::Row>,
    groups: Option<Vec<TableGroup>>,
    group_by: Option<PropertyOrder>,
    manual_groups: bool,
}

#[derive(Serialize)]
struct TableGroup {
    label: String,
    rows: Vec<note_views::Row>,
}

pub fn is_base(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("base"))
}

fn column(key: &str) -> bool {
    matches!(key, FILE_NAME | "file.path" | "file.folder" | "file.ext") ||
        key.strip_prefix("note.").is_some_and(|name| !name.trim().is_empty())
}

fn view_columns(view: &Yaml) -> Result<Vec<String>, String> {
    let order = &view["order"];
    if order.is_badvalue() { return Ok(vec![FILE_NAME.into()]); }
    let order = order.as_vec().filter(|order| order.len() <= MAX_COLUMNS).ok_or("Invalid table column order")?;
    if order.is_empty() { return Ok(vec![FILE_NAME.into()]); }
    order.iter().map(|value| {
        let key = value.as_str().ok_or("Invalid table column")?;
        let key = property_key(key);
        column(&key).then_some(key).ok_or_else(|| "This base uses an unsupported column".into())
    }).collect()
}

fn property_key(key: &str) -> String {
    if ["file.", "formula.", "note."].iter().any(|prefix| key.starts_with(prefix)) {
        key.to_owned()
    } else {
        format!("note.{key}")
    }
}

fn base_path(path: &str) -> Result<PathBuf, String> {
    safe_rel(path).filter(|rel| is_base(rel)).ok_or_else(|| "Invalid .base path".into())
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CsvSeparator {
    Comma,
    Semicolon,
}

fn csv_contents(headers: Vec<String>, rows: Vec<Vec<String>>, separator: CsvSeparator) -> Result<String, String> {
    if headers.is_empty() || headers.len() > MAX_COLUMNS || rows.iter().any(|row| row.len() != headers.len()) {
        return Err("The CSV rows must match the table columns".into());
    }
    let separator = match separator {
        CsvSeparator::Comma => ',',
        CsvSeparator::Semicolon => ';',
    };
    let mut csv = String::from('\u{feff}');
    for row in std::iter::once(headers).chain(rows) {
        for (index, value) in row.into_iter().enumerate() {
            if index != 0 { csv.push(separator); }
            csv.push('"');
            // Quoting alone does not stop spreadsheet apps from evaluating a cell.
            let trimmed = value.trim_start_matches(|ch: char| ch.is_whitespace() || ch.is_control() || ch == '\u{feff}');
            let starts_formula = trimmed.starts_with(['=', '+', '-', '@', '＝', '＋', '－', '＠']);
            let plain_number = trimmed.starts_with(['+', '-'])
                && trimmed.parse::<f64>().is_ok_and(f64::is_finite);
            if (starts_formula && !plain_number) || value.starts_with(['\t', '\r', '\n', '\0']) {
                csv.push('\'');
            }
            for ch in value.chars() {
                if ch == '"' { csv.push('"'); }
                csv.push(ch);
            }
            csv.push('"');
            if csv.len() > MAX_CSV_BYTES { return Err("The CSV would exceed 32 MiB".into()); }
        }
        csv.push_str("\r\n");
    }
    if csv.len() > MAX_CSV_BYTES { return Err("The CSV would exceed 32 MiB".into()); }
    Ok(csv)
}

pub fn export_csv(
    root: &Path, base: &str, filename: &str, headers: Vec<String>, rows: Vec<Vec<String>>, separator: CsvSeparator,
) -> Result<String, String> {
    let base = base_path(base)?;
    let filename = safe_rel(filename).filter(|path| {
        path.components().count() == 1 && path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("csv"))
    }).ok_or("Enter a valid CSV file name")?;
    let vault = Vault::open(root).map_err(|error| error.to_string())?;
    if !vault.is_file(&base) { return Err("The base could not be found".into()); }
    let destination = base.with_file_name(filename);
    let contents = csv_contents(headers, rows, separator)?;
    vault.write_new_atomic(&destination, contents.as_bytes()).map_err(|error| {
        if error.kind() == ErrorKind::AlreadyExists {
            "A file with that name already exists. Choose another name.".to_owned()
        } else {
            format!("Could not export CSV: {error}")
        }
    })?;
    Ok(opensidian_lib::vault_path_string(&destination))
}

pub fn list(root: &Path) -> Result<Vec<String>, String> {
    let vault = Vault::open(root).map_err(|error| error.to_string())?;
    let mut pending = vec![(PathBuf::new(), 0)];
    let mut paths = Vec::new();
    while let Some((folder, depth)) = pending.pop() {
        let entries = match vault.p_list(&folder) {
            Ok(entries) => entries,
            Err(error) if folder.as_os_str().is_empty() => return Err(error.to_string()),
            Err(_) => continue,
        };
        for (name, kind) in entries {
            if name.starts_with('.') || !crate::vaultfs::valid_component(name.as_ref()) { continue; }
            let rel = folder.join(name);
            if kind == "folder" && depth < crate::vaultfs::MAX_WALK_DEPTH {
                pending.push((rel, depth + 1));
            } else if kind == "file" && is_base(&rel) {
                paths.push(opensidian_lib::vault_path_string(&rel));
            }
        }
    }
    paths.sort_unstable();
    Ok(paths)
}

pub fn create(root: &Path, path: &str, config: NewBase) -> Result<(), String> {
    let path = base_path(path)?;
    if config.columns.is_empty() || config.columns.len() > MAX_COLUMNS || !config.columns.iter().all(|key| column(key))
        || !config.columns.contains(&config.sort) {
        return Err("Choose supported columns and a sort column".into());
    }
    // JSON is a valid YAML representation and safely quotes arbitrary property names.
    let mut filters = vec!["file.ext == \"md\"".to_owned()];
    if let Some(folder) = config.folder {
        let folder = safe_rel(&folder).ok_or("Invalid base folder filter")?;
        let literal = serde_json::to_string(&opensidian_lib::vault_path_string(&folder)).map_err(|error| error.to_string())?;
        filters.push(format!("file.inFolder({literal})"));
    }
    if config.conditions.len() > 16 { return Err("A base can save up to sixteen property conditions".into()); }
    for condition in config.conditions { filters.push(condition.expression()?); }
    let mut base = serde_json::json!({
        "filters": { "and": filters },
        "views": [{
            "type": "table", "name": "Notes", "order": config.columns,
            "sort": [{ "property": config.sort, "direction": if config.descending { "DESC" } else { "ASC" } }]
        }]
    });
    if let Some(group) = config.group_by {
        if !column(&group.property) { return Err("Choose a supported grouping property".into()); }
        base["views"][0]["groupBy"] = serde_json::to_value(group).map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(&base).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_BYTES { return Err("The base is too large".into()); }
    let vault = Vault::open(root).map_err(|error| error.to_string())?;
    vault.write_new_atomic(&path, &bytes).map_err(|error| format!("Could not create the base: {error}"))
}

enum Filter {
    All(Vec<Self>),
    Any(Vec<Self>),
    None(Vec<Self>),
    Extension(String, bool),
    Folder(String),
    Property(Yaml, Operator, Yaml),
}

impl Filter {
    fn parse(yaml: &Yaml, depth: usize) -> Result<Self, String> {
        let invalid = || "Unsupported base filter. Use a folder, extension or property comparison with and, or, or not.".to_owned();
        if depth > 16 { return Err(invalid()); }
        if yaml.is_badvalue() || yaml.is_null() { return Ok(Self::All(Vec::new())); }
        if let Some(hash) = yaml.as_hash().filter(|hash| hash.len() == 1) {
            let (key, children) = hash.iter().next().unwrap();
            let children = children.as_vec().ok_or_else(invalid)?.iter()
                .map(|child| Self::parse(child, depth + 1)).collect::<Result<Vec<_>, _>>()?;
            return match key.as_str() {
                Some("and") => Ok(Self::All(children)),
                Some("or") => Ok(Self::Any(children)),
                Some("not") => Ok(Self::None(children)),
                _ => Err(invalid()),
            };
        }
        let expr = yaml.as_str().ok_or_else(invalid)?.trim();
        let parse_string = |value: &str| {
            let value = value.trim();
            if value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2 {
                let inner = &value[1..value.len() - 1];
                return (!inner.contains('\'')).then(|| inner.to_owned()).ok_or_else(invalid);
            }
            serde_json::from_str::<String>(value).map_err(|_| invalid())
        };
        if let Some(folder) = expr.strip_prefix("file.inFolder(").and_then(|s| s.strip_suffix(')')) {
            return Ok(Self::Folder(parse_string(folder)?.trim_matches('/').to_owned()));
        }
        let (key, comparison) = if let Some(rest) = expr.strip_prefix("note[") {
            // Decode the quoted key before finding the closing bracket, which can also occur in the key.
            let quoted = rest.trim_start();
            let mut reader = serde_json::Deserializer::from_str(quoted).into_iter::<String>();
            let key = reader.next().ok_or_else(invalid)?.map_err(|_| invalid())?;
            let comparison = quoted[reader.byte_offset()..].trim_start().strip_prefix(']').ok_or_else(invalid)?;
            (key, comparison.trim_start())
        } else {
            let at = expr.find(['=', '!', '<', '>']).ok_or_else(invalid)?;
            let key = expr[..at].trim();
            let comparison = &expr[at..];
            if key == "file.ext" {
                for (op, equal) in [("==", true), ("!=", false)] {
                    if let Some(value) = comparison.strip_prefix(op) { return Ok(Self::Extension(parse_string(value)?, equal)); }
                }
                return Err(invalid());
            }
            let key = key.strip_prefix("note.").unwrap_or(key);
            if key.is_empty() || !key.chars().all(|ch| ch.is_alphanumeric() || ch == '_') { return Err(invalid()); }
            (key.to_owned(), comparison)
        };
        for (operator, symbol) in Operator::ALL {
            let Some(literal) = comparison.strip_prefix(symbol) else { continue };
            let literal = literal.trim();
            let value = if literal.starts_with(['"', '\'']) {
                Yaml::String(parse_string(literal)?)
            } else if matches!(literal, "true" | "false" | "null") {
                Yaml::from_str(literal)
            } else {
                let number = literal.parse::<serde_json::Number>().map_err(|_| invalid())?;
                if number.is_u64() && number.as_i64().is_none() { return Err(invalid()); }
                Yaml::from_str(literal)
            };
            if !matches!(operator, Operator::Eq | Operator::Ne) && numeric(&value).is_none() {
                return Err("Use a number for greater-than and less-than conditions".into());
            }
            return Ok(Self::Property(Yaml::String(key), operator, value));
        }
        Err(invalid())
    }

    fn matches(&self, note: &str, properties: &yaml_rust2::yaml::Hash) -> bool {
        match self {
            Self::All(filters) => filters.iter().all(|filter| filter.matches(note, properties)),
            Self::Any(filters) => filters.iter().any(|filter| filter.matches(note, properties)),
            Self::None(filters) => !filters.iter().any(|filter| filter.matches(note, properties)),
            Self::Extension(ext, equal) => (ext == "md") == *equal,
            Self::Folder(folder) => {
                let parent = note.rsplit_once('/').map_or("", |(parent, _)| parent);
                folder.is_empty() || parent == folder || parent.strip_prefix(folder).is_some_and(|s| s.starts_with('/'))
            }
            Self::Property(key, operator, expected) => {
                let actual = properties.get(key).unwrap_or(&Yaml::Null);
                let order = match (actual, expected) {
                    (Yaml::Integer(a), Yaml::Integer(b)) => Some(a.cmp(b)),
                    (a, b) if numeric(b).is_some() => numeric(a).zip(numeric(b)).and_then(|(a, b)| a.partial_cmp(&b)),
                    (a, b) if a == b => Some(Ordering::Equal),
                    _ => None,
                };
                operator.test(order)
            }
        }
    }
}

fn numeric(value: &Yaml) -> Option<f64> {
    match value {
        Yaml::Integer(number) => Some(*number as f64),
        Yaml::Real(number) => number.parse::<f64>().ok().filter(|number| number.is_finite()),
        _ => None,
    }
}

fn group_key(value: &Yaml) -> Yaml {
    if let Yaml::Real(_) = value {
        if let Some(number) = numeric(value) {
            let integer = number as i64;
            if number < i64::MAX as f64 && integer as f64 == number {
                return Yaml::Integer(integer);
            }
            return Yaml::Real(number.to_string());
        }
    }
    value.clone()
}

fn group_value(name: &str, properties: &yaml_rust2::yaml::Hash, key: &str) -> Yaml {
    let text = match key {
        FILE_NAME => name.rsplit('/').next().unwrap_or(name).to_owned(),
        "file.path" => format!("{name}.md"),
        "file.folder" => name.rsplit_once('/').map_or("", |(folder, _)| folder).to_owned(),
        "file.ext" => "md".to_owned(),
        key => return group_key(properties.get(&Yaml::String(key["note.".len()..].into())).unwrap_or(&Yaml::Null)),
    };
    Yaml::String(text)
}

fn group_order(a: &Yaml, b: &Yaml) -> Ordering {
    let rank = |value: &Yaml| match value {
        Yaml::Boolean(_) => 0,
        Yaml::Integer(_) | Yaml::Real(_) => 1,
        Yaml::String(_) => 2,
        _ => 3,
    };
    rank(a).cmp(&rank(b)).then_with(|| {
        match (numeric(a), numeric(b)) {
            (Some(a), Some(b)) => a.total_cmp(&b),
            (a, b) => a.is_some().cmp(&b.is_some()),
        }
    }).then_with(|| a.cmp(b))
}

fn grouping(view: &Yaml) -> Result<Option<PropertyOrder>, String> {
    let by = &view["groupBy"];
    if by.is_badvalue() {
        if !view["groupOrder"].is_badvalue() { return Err("Group order requires a grouping property".into()); }
        return Ok(None);
    }
    let property = by["property"].as_str().ok_or("Invalid grouping property")?;
    let property = property_key(property);
    if !column(&property) { return Err("Unsupported grouping property".into()); }
    let direction = match by["direction"].as_str() {
        Some("DESC") => Direction::Descending,
        Some("ASC") => Direction::Ascending,
        None if by["direction"].is_badvalue() => Direction::Ascending,
        _ => return Err("Unsupported group direction".into()),
    };
    Ok(Some(PropertyOrder { property, direction }))
}

fn sorting(view: &Yaml) -> Result<Vec<PropertyOrder>, String> {
    let sort = &view["sort"];
    if sort.is_badvalue() { return Ok(Vec::new()); }
    sort.as_vec().ok_or("Invalid base sort")?.iter().map(|key| {
        let property = key["property"].as_str().ok_or("Invalid base sort property")?;
        let property = property_key(property);
        if !column(&property) { return Err("Unsupported base sort property".into()); }
        let direction = match key["direction"].as_str() {
            Some("ASC") => Direction::Ascending,
            Some("DESC") => Direction::Descending,
            _ => return Err("Unsupported base sort direction".into()),
        };
        Ok(PropertyOrder { property, direction })
    }).collect()
}

fn grouped_rows<'a>(
    grouping: Option<&PropertyOrder>,
    order: &Yaml,
    docs: impl Iterator<Item = &'a (&'a str, note_views::Row, yaml_rust2::yaml::Hash)>,
) -> Result<(Vec<note_views::Row>, Option<Vec<TableGroup>>), String> {
    let Some(grouping) = grouping else {
        return Ok((docs.map(|(_, row, _)| row.clone()).collect(), None));
    };
    let descending = grouping.direction == Direction::Descending;
    let mut buckets = BTreeMap::<_, Vec<_>>::new();
    for (name, row, properties) in docs {
        buckets.entry(group_value(name, properties, &grouping.property)).or_default().push(row.clone());
    }
    let groups = if order.is_badvalue() {
        let mut groups: Vec<_> = buckets.into_iter().collect();
        groups.sort_by(|(a, _), (b, _)| {
            // Keep the group for notes with no value at the end in either direction.
            a.is_null().cmp(&b.is_null()).then_with(|| {
                if descending { group_order(b, a) } else { group_order(a, b) }
            })
        });
        groups
    } else {
        let order = order.as_vec().ok_or("Invalid group order")?;
        let mut seen = BTreeSet::new();
        order.iter().map(group_key).filter_map(|key| {
            seen.insert(key.clone()).then(|| {
                let rows = buckets.remove(&key).unwrap_or_default();
                (key, rows)
            })
        }).collect()
    };
    let groups = groups.into_iter().map(|(key, rows)| {
        let label = match &key {
            Yaml::Null => "(No value)".to_owned(),
            Yaml::String(text) if text.is_empty() => "(Empty value)".to_owned(),
            // Keep numeric and checkbox values distinguishable from their text equivalents.
            Yaml::String(text) if numeric(&Yaml::from_str(text)).is_some()
                || matches!(text.as_str(), "true" | "false" | "(No value)" | "(Empty value)") => format!("{text:?}"),
            _ => note_views::display(&key),
        };
        TableGroup { label, rows }
    }).collect();
    Ok((Vec::new(), Some(groups)))
}

fn parse(bytes: &[u8]) -> Result<Yaml, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "The base is not valid UTF-8")?;
    let mut docs = YamlLoader::load_from_str(text).map_err(|error| error.to_string())?;
    if docs.len() != 1 { return Err("The base must contain one YAML document".into()); }
    Ok(docs.pop().unwrap())
}

pub fn open(root: &Path, index: &Index, path: &str) -> Result<Base, String> {
    let path = base_path(path)?;
    let vault = Vault::open(root).map_err(|error| error.to_string())?;
    let bytes = vault.p_read(&path, MAX_BYTES).map_err(|error| error.to_string())?;
    render_base(&parse(&bytes)?, index, format!("{:x}", Sha256::digest(&bytes)))
}

pub fn set_grouping(
    root: &Path, index: &Index, path: &str, revision: &str, id: usize, group_by: Option<PropertyOrder>,
) -> Result<Base, String> {
    edit_view(root, index, path, revision, id, |view| {
        if grouping(view)? == group_by && view["groupOrder"].is_badvalue() { return Ok(()); }
        let Yaml::Hash(settings) = view else { return Err("Invalid table view".into()); };
        let key = Yaml::String("groupBy".into());
        if let Some(group) = group_by {
            if !column(&group.property) { return Err("Choose a supported grouping property".into()); }
            let group = serde_json::to_vec(&group).map_err(|error| error.to_string())?;
            settings.insert(key, parse(&group)?);
        } else {
            settings.remove(&key);
        }
        settings.remove(&Yaml::String("groupOrder".into()));
        Ok(())
    })
}

pub fn set_sorting(
    root: &Path, index: &Index, path: &str, revision: &str, id: usize, sort: Vec<PropertyOrder>,
) -> Result<Base, String> {
    edit_view(root, index, path, revision, id, |view| {
        if sorting(view)? == sort { return Ok(()); }
        let mut seen = BTreeSet::new();
        if !sort.iter().all(|key| column(&key.property) && seen.insert(&key.property)) {
            return Err("Choose distinct supported sorting properties".into());
        }
        let Yaml::Hash(settings) = view else { return Err("Invalid table view".into()); };
        let key = Yaml::String("sort".into());
        if sort.is_empty() {
            settings.remove(&key);
        } else {
            let sort = serde_json::to_vec(&sort).map_err(|error| error.to_string())?;
            settings.insert(key, parse(&sort)?);
        }
        Ok(())
    })
}

pub fn set_columns(
    root: &Path, index: &Index, path: &str, revision: &str, id: usize, columns: Vec<String>,
) -> Result<Base, String> {
    edit_view(root, index, path, revision, id, |view| {
        let mut seen = BTreeSet::new();
        if columns.is_empty() || columns.len() > MAX_COLUMNS
            || !columns.iter().all(|key| column(key) && seen.insert(key)) {
            return Err("Choose between one and 128 distinct supported columns".into());
        }
        if view_columns(view)? == columns { return Ok(()); }
        let Yaml::Hash(settings) = view else { return Err("Invalid table view".into()); };
        let order = Yaml::Array(columns.into_iter().map(Yaml::String).collect());
        settings.insert(Yaml::String("order".into()), order);
        Ok(())
    })
}

fn edit_view(
    root: &Path, index: &Index, path: &str, revision: &str, id: usize,
    edit: impl FnOnce(&mut Yaml) -> Result<(), String>,
) -> Result<Base, String> {
    let path = base_path(path)?;
    let vault = Vault::open(root).map_err(|error| error.to_string())?;
    let original = vault.p_read(&path, MAX_BYTES).map_err(|error| error.to_string())?;
    let conflict = "The base changed on disk. Reopen it and try again.";
    if revision != format!("{:x}", Sha256::digest(&original)) { return Err(conflict.into()); }
    let mut base = parse(&original)?;
    let Yaml::Hash(root) = &mut base else { return Err("Invalid base".into()); };
    let Some(Yaml::Array(views)) = root.get_mut(&Yaml::String("views".into())) else {
        return Err("Invalid base views".into());
    };
    let view = views.get_mut(id).filter(|view| view["type"].as_str() == Some("table"))
        .ok_or("The table view could not be found")?;
    let previous = view.clone();
    edit(view)?;
    if *view == previous {
        return render_base(&base, index, revision.into());
    }
    let mut yaml = String::new();
    YamlEmitter::new(&mut yaml).dump(&base).map_err(|error| error.to_string())?;
    yaml.push('\n');
    if yaml.len() as u64 > MAX_BYTES { return Err("The base is too large".into()); }
    if parse(yaml.as_bytes())? != base { return Err("The base could not be saved without changing its YAML values".into()); }
    let updated = render_base(&base, index, format!("{:x}", Sha256::digest(yaml.as_bytes())))?;
    if !vault.replace_if_unchanged(&path, &original, yaml.as_bytes()).map_err(|error| error.to_string())? {
        return Err(conflict.into());
    }
    Ok(updated)
}

fn render_base(base: &Yaml, index: &Index, revision: String) -> Result<Base, String> {
    let global = Filter::parse(&base["filters"], 0)?;
    let views = base["views"].as_vec().filter(|views| !views.is_empty() && views.len() <= 16)
        .ok_or("The base must contain between one and sixteen views")?;
    let mut properties = BTreeSet::new();
    let docs: Vec<_> = index.docs().filter_map(|(name, text, _)| {
        let note = note_views::properties(text);
        properties.extend(note.keys().filter_map(|key| key.as_str().map(str::to_owned)));
        global.matches(name, &note).then(|| (name, note_views::row_from_properties(name, &note), note))
    }).collect();
    let labels: BTreeMap<_, _> = base["properties"].as_hash().into_iter().flatten().filter_map(|(key, settings)| {
        let key = key.as_str()?;
        let label = settings["displayName"].as_str()?;
        let key = property_key(key);
        if !column(&key) { return None; }
        let canonical = base["properties"][key.as_str()]["displayName"].as_str().unwrap_or(label);
        Some((key, canonical.to_owned()))
    }).collect();
    let views = views.iter().enumerate().filter(|(_, view)| view["type"].as_str() == Some("table")).map(|(id, view)| {
        if !view["summaries"].is_badvalue() {
            return Err("Summaries are not supported in base tables yet".into());
        }
        let columns = view_columns(view)?;
        let mut labels = labels.clone();
        for key in &columns {
            if let Some(display) = key.strip_prefix("note.").and_then(|name| base["properties"][name]["displayName"].as_str()) {
                labels.entry(key.clone()).or_insert_with(|| display.to_owned());
            }
        }
        let sort = sorting(view)?;
        let filter = Filter::parse(&view["filters"], 0)?;
        let matching = docs.iter().filter(|(name, _, properties)| filter.matches(name, properties));
        let group_by = grouping(view)?;
        let order = &view["groupOrder"];
        let (rows, groups) = grouped_rows(group_by.as_ref(), order, matching)?;
        let manual_groups = !order.is_badvalue();
        let limit = view["limit"].as_i64().and_then(|limit| usize::try_from(limit).ok());
        Ok(Table { id, title: view["name"].as_str().unwrap_or("Notes").to_owned(), columns, sort, limit, labels, rows, groups, group_by, manual_groups })
    }).collect::<Result<Vec<_>, String>>()?;
    if views.is_empty() { return Err("This base has no supported table view".into()); }
    Ok(Base { revision, properties, views })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn creates_lists_and_opens_saved_note_tables() {
        let root = std::env::temp_dir().join(format!("opensidian-base-{}", opensidian_lib::os_random_hex().unwrap()));
        fs::create_dir_all(root.join("Project/Sub")).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
        }
        let _cleanup = Cleanup(root.clone());
        fs::write(root.join("Project/Sub/Alpha.md"), "---\npriority: 2\n---\n# Alpha").unwrap();
        fs::write(root.join("Other.md"), "# Other").unwrap();
        let config = || NewBase { columns: vec![FILE_NAME.into(), "note.priority".into()], sort: "note.priority".into(), descending: true, folder: None, conditions: vec![], group_by: None };
        create(&root, "Project/Work.base", config()).unwrap();
        let original = fs::read(root.join("Project/Work.base")).unwrap();
        assert!(create(&root, "Project/Work.base", config()).is_err());
        assert_eq!(fs::read(root.join("Project/Work.base")).unwrap(), original);
        assert_eq!(list(&root).unwrap(), ["Project/Work.base"]);
        let mut index = Index::build(&root);
        let tables = open(&root, &index, "Project/Work.base").unwrap().views;
        assert_eq!(tables[0].columns, [FILE_NAME, "note.priority"]);
        assert_eq!(tables[0].sort[0].property, "note.priority");
        assert!(tables[0].sort[0].direction == Direction::Descending);
        assert_eq!(tables[0].rows.len(), 2);
        index.upsert("Project/Sub/Alpha", "---\npriority: 7\n---\n");
        let tables = open(&root, &index, "Project/Work.base").unwrap().views;
        assert_eq!(serde_json::to_value(&tables[0].rows[1]).unwrap()["values"]["priority"], "7");
        create(&root, "Project/Scoped.base", NewBase { folder: Some("Project".into()), ..config() }).unwrap();
        assert_eq!(open(&root, &index, "Project/Scoped.base").unwrap().views[0].rows.len(), 1);

        let yaml = "filters: 'file.inFolder(\"Project\")'\nviews:\n  - type: table\n    name: Project notes\n    order: [file.name, note.priority]\n    limit: 3\n";
        fs::write(root.join("Existing.base"), yaml).unwrap();
        let tables = open(&root, &index, "Existing.base").unwrap().views;
        assert_eq!(tables[0].title, "Project notes");
        assert_eq!(tables[0].limit, Some(3));
        assert_eq!(tables[0].rows.len(), 1);
        fs::write(root.join("Unsupported.base"), yaml.replace("file.name", "formula.total")).unwrap();
        assert!(open(&root, &index, "Unsupported.base").is_err());
        assert!(open(&root, &index, "../Outside.base").is_err());

        index.upsert("Project/Text", "---\npriority: \"7\"\n'has ] space': active\ndone: true\n---\n");
        index.upsert("Project/Ready", "---\npriority: 8.5\n'has ] space': active\ndone: true\n---\n");
        index.upsert("Project/Unchecked", "---\npriority: 9\n'has ] space': active\ndone: false\n---\n");
        let conditions = vec![
            Condition { property: "priority".into(), operator: Operator::Ge, kind: ValueKind::Number, value: "7".into() },
            Condition { property: "has ] space".into(), operator: Operator::Eq, kind: ValueKind::Text, value: "active".into() },
            Condition { property: "done".into(), operator: Operator::Eq, kind: ValueKind::Checkbox, value: "true".into() },
        ];
        create(&root, "Filtered.base", NewBase { conditions, ..config() }).unwrap();
        let table = &open(&root, &index, "Filtered.base").unwrap().views[0];
        assert_eq!(table.rows.len(), 1);
        assert_eq!(serde_json::to_value(&table.rows[0]).unwrap()["name"], "Project/Ready");
        index.upsert("Project/Ready", "---\npriority: 2\n---\n");
        assert!(open(&root, &index, "Filtered.base").unwrap().views[0].rows.is_empty());

        create(&root, "Grouped.base", NewBase {
            group_by: Some(PropertyOrder { property: "note.priority".into(), direction: Direction::Descending }),
            ..config()
        }).unwrap();
        let grouped = open(&root, &index, "Grouped.base").unwrap().views;
        assert!(grouped[0].rows.is_empty());
        let groups = grouped[0].groups.as_ref().unwrap();
        assert_eq!(groups.iter().map(|group| group.label.as_str()).collect::<Vec<_>>(),
            ["\"7\"", "9", "7", "2", "(No value)"]);
        index.upsert("Project/Tied", "---\npriority: 7.0\n---\n");
        let yaml = "views:\n  - type: table\n    filters: 'file.inFolder(\"Project\")'\n    groupBy: { property: priority, direction: ASC }\n    groupOrder: [null, 7, 7.0, Future]\n";
        fs::write(root.join("Manual.base"), yaml).unwrap();
        let manual = open(&root, &index, "Manual.base").unwrap().views;
        let groups = manual[0].groups.as_ref().unwrap();
        assert_eq!(groups.iter().map(|group| (group.label.as_str(), group.rows.len())).collect::<Vec<_>>(),
            [("(No value)", 0), ("7", 2), ("Future", 0)]);
        fs::write(root.join("Manual.base"), yaml.replace("[null, 7, 7.0, Future]", "[]")).unwrap();
        assert!(open(&root, &index, "Manual.base").unwrap().views[0].groups.as_ref().unwrap().is_empty());
        fs::write(root.join("Manual.base"), yaml.replace("property: priority", "property: file.folder")
            .replace("    groupOrder: [null, 7, 7.0, Future]\n", "")).unwrap();
        let folders = open(&root, &index, "Manual.base").unwrap().views;
        assert_eq!(folders[0].groups.as_ref().unwrap().iter().map(|group| group.label.as_str()).collect::<Vec<_>>(),
            ["Project", "Project/Sub"]);
    }

    #[test]
    fn edits_only_the_selected_table_and_rejects_stale_saves() {
        let root = std::env::temp_dir().join(format!("opensidian-base-edit-{}", opensidian_lib::os_random_hex().unwrap()));
        fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
        }
        let _cleanup = Cleanup(root.clone());
        let path = "Project.base";
        let file = root.join(path);
        let yaml = r#"# A base with additional settings
filters: 'file.ext == "md"'
properties: {status: {displayName: "Workflow"}}
views:
  - type: cards
    name: Untouched
    groupBy: {property: note.status, direction: ASC}
    groupOrder: [todo, done]
  - type: table
    name: Project notes
    order: [file.name, note.status]
    groupBy: {property: note.status, direction: ASC}
    groupOrder: [todo]
    extra: {custom: [true, "08", null]}
  - type: table
    name: Other notes
"#;
        fs::write(&file, yaml).unwrap();
        let original = parse(yaml.as_bytes()).unwrap();
        let mut index = Index::build(&root);
        index.upsert("Tasks/One", "---\nstatus: todo\n---\n");
        index.upsert("Tasks/Two", "---\nstatus: done\n---\n");
        let opened = open(&root, &index, path).unwrap();
        assert_eq!(opened.views[0].id, 1);
        assert!(opened.views[0].manual_groups);
        assert!(opened.properties.contains("status"));
        let group = || PropertyOrder { property: "note.status".into(), direction: Direction::Ascending };
        assert!(set_grouping(&root, &index, path, &opened.revision, 0, Some(group())).is_err());
        assert_eq!(fs::read(&file).unwrap(), yaml.as_bytes());
        let saved = set_grouping(&root, &index, path, &opened.revision, 1, Some(group())).unwrap();
        assert!(!saved.views[0].manual_groups);
        let groups = saved.views[0].groups.as_ref().unwrap();
        assert_eq!(groups.iter().map(|group| group.label.as_str()).collect::<Vec<_>>(), ["done", "todo"]);
        let saved_bytes = fs::read(&file).unwrap();
        let parsed = parse(&saved_bytes).unwrap();
        assert_eq!(parsed["filters"], original["filters"]);
        assert_eq!(parsed["properties"], original["properties"]);
        assert_eq!(parsed["views"][0], original["views"][0]);
        assert_eq!(parsed["views"][1]["extra"], original["views"][1]["extra"]);
        assert_eq!(parsed["views"][2], original["views"][2]);
        assert_eq!(open(&root, &index, path).unwrap().revision, saved.revision);
        assert!(set_grouping(&root, &index, path, &opened.revision, 1, None).is_err());
        assert_eq!(fs::read(&file).unwrap(), saved_bytes);
        let no_op = set_grouping(&root, &index, path, &saved.revision, 1, Some(group())).unwrap();
        assert_eq!(no_op.revision, saved.revision);
        let ungrouped = set_grouping(&root, &index, path, &saved.revision, 1, None).unwrap();
        assert!(ungrouped.views[0].groups.is_none());
        assert_eq!(ungrouped.views[0].rows.len(), 2);
    }

    #[test]
    fn applies_view_filters_and_rejects_unsupported_expressions() {
        let yaml = YamlLoader::load_from_str("or:\n  - 'note.done == true'\n  - 'priority < 3'\n").unwrap();
        let filter = Filter::parse(&yaml[0], 0).unwrap();
        assert!(filter.matches("Note", &note_views::properties("---\npriority: 2\n---\n")));
        assert!(filter.matches("Note", &note_views::properties("---\ndone: true\n---\n")));
        assert!(!filter.matches("Note", &note_views::properties("---\ndone: \"true\"\n---\n")));
        for expr in ["file.name == 1", "priority > \"3\"", "note.done == true || true", "note.priority == 1e999"] {
            assert!(Filter::parse(&Yaml::String(expr.into()), 0).is_err(), "{expr}");
        }
    }

    #[test]
    fn loads_and_saves_sort_rules_in_priority_order() {
        let root = std::env::temp_dir().join(format!("opensidian-base-sort-{}", opensidian_lib::os_random_hex().unwrap()));
        fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
        }
        let _cleanup = Cleanup(root.clone());
        let path = "Notes.base";
        let yaml = r#"views:
  - type: table
    name: Projects
    sort:
      - {property: priority, direction: DESC}
      - {property: note.date, direction: ASC}
    groupBy: {property: note.status, direction: ASC}
    groupOrder: [todo, done]
  - type: table
    name: All notes
"#;
        let file = root.join(path);
        fs::write(&file, yaml).unwrap();
        let index = Index::build(&root);
        let opened = open(&root, &index, path).unwrap();
        let sort = &opened.views[0].sort;
        assert_eq!(sort.iter().map(|rule| rule.property.as_str()).collect::<Vec<_>>(), ["note.priority", "note.date"]);
        assert!(sort[0].direction == Direction::Descending);
        assert!(opened.views[1].sort.is_empty());
        let rule = |property: &str, direction| PropertyOrder { property: property.into(), direction };
        let invalid = vec![rule("note.date", Direction::Ascending), rule("note.date", Direction::Descending)];
        assert!(set_sorting(&root, &index, path, &opened.revision, 0, invalid).is_err());
        assert_eq!(fs::read(&file).unwrap(), yaml.as_bytes());
        let changed = vec![rule("note.date", Direction::Descending), rule("note.priority", Direction::Ascending)];
        let updated = set_sorting(&root, &index, path, &opened.revision, 0, changed).unwrap();
        let reopened = open(&root, &index, path).unwrap();
        assert_eq!(serde_json::to_value(&reopened.views[0].sort).unwrap(), serde_json::json!([
            { "property": "note.date", "direction": "DESC" },
            { "property": "note.priority", "direction": "ASC" },
        ]));
        let old = parse(yaml.as_bytes()).unwrap();
        let current = parse(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(current["views"][0]["groupOrder"], old["views"][0]["groupOrder"]);
        assert_eq!(current["views"][1], old["views"][1]);
        assert!(set_sorting(&root, &index, path, &opened.revision, 0, vec![]).is_err());
        assert!(set_sorting(&root, &index, path, &updated.revision, 0, vec![]).unwrap().views[0].sort.is_empty());
    }

    #[test]
    fn exports_spreadsheet_ready_csv_without_overwriting_files() {
        let root = std::env::temp_dir().join(format!("opensidian-base-csv-{}", opensidian_lib::os_random_hex().unwrap()));
        fs::create_dir_all(root.join("Proyecto")).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
        }
        let _cleanup = Cleanup(root.clone());
        let base = "Proyecto/Notas.base";
        fs::write(root.join(base), "views: [{type: table}]").unwrap();
        let headers = || vec!["Nota".into(), "Detalle".into()];
        let rows = || vec![
            vec!["Café, té".into(), "Dijo \"hola\"\nAdiós".into()],
            vec!["-3.5".into(), "  =1+1".into()],
            vec!["".into(), "@pendiente".into()],
        ];
        let saved = export_csv(&root, base, "Informe.csv", headers(), rows(), CsvSeparator::Semicolon).unwrap();
        assert_eq!(saved, "Proyecto/Informe.csv");
        let expected = "\u{feff}\"Nota\";\"Detalle\"\r\n\"Café, té\";\"Dijo \"\"hola\"\"\nAdiós\"\r\n\"-3.5\";\"'  =1+1\"\r\n\"\";\"'@pendiente\"\r\n";
        let file = root.join(saved);
        assert_eq!(fs::read(&file).unwrap(), expected.as_bytes());
        assert!(export_csv(&root, base, "Informe.csv", headers(), vec![], CsvSeparator::Comma).unwrap_err().contains("already exists"));
        assert_eq!(fs::read(&file).unwrap(), expected.as_bytes());
        assert_eq!(csv_contents(headers(), vec![], CsvSeparator::Comma).unwrap(), "\u{feff}\"Nota\",\"Detalle\"\r\n");
        assert!(csv_contents(headers(), vec![vec!["Short".into()]], CsvSeparator::Comma).is_err());
        for invalid in ["../../Otro.csv", "C:/Otro.csv", "Proyecto/Otro.csv", "Informe.txt", "NUL.csv"] {
            assert!(export_csv(&root, base, invalid, headers(), rows(), CsvSeparator::Comma).is_err(), "{invalid}");
        }
    }

    #[test]
    fn edits_visible_columns_in_order_without_changing_other_view_settings() {
        let root = std::env::temp_dir().join(format!("opensidian-base-columns-{}", opensidian_lib::os_random_hex().unwrap()));
        fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
        }
        let _cleanup = Cleanup(root.clone());
        let path = "Work.base";
        let yaml = r#"properties:
  project.status: {displayName: Status}
  note.project.status: {displayName: Workflow}
views:
  - type: table
    order: [file.name, project.status]
    sort: [{property: project.status, direction: ASC}]
    limit: 10
  - type: table
    order: [file.folder]
"#;
        let file = root.join(path);
        fs::write(&file, yaml).unwrap();
        let index = Index::build(&root);
        let opened = open(&root, &index, path).unwrap();
        assert_eq!(opened.views[0].columns, ["file.name", "note.project.status"]);
        assert_eq!(opened.views[0].labels["note.project.status"], "Workflow");
        let unchanged = set_columns(&root, &index, path, &opened.revision, 0, opened.views[0].columns.clone()).unwrap();
        assert_eq!(unchanged.revision, opened.revision);
        for invalid in [vec![], vec!["file.name".into(); 2], vec!["formula.score".into()]] {
            assert!(set_columns(&root, &index, path, &opened.revision, 0, invalid).is_err());
        }
        assert_eq!(fs::read(&file).unwrap(), yaml.as_bytes());
        let updated = set_columns(&root, &index, path, &opened.revision, 0,
            vec!["file.folder".into(), "file.name".into()]).unwrap();
        assert_eq!(updated.views[0].columns, ["file.folder", "file.name"]);
        assert_eq!(updated.views[0].sort[0].property, "note.project.status");
        assert_eq!(updated.views[0].limit, Some(10));
        assert_eq!(updated.views[0].labels["note.project.status"], "Workflow");
        let reopened = open(&root, &index, path).unwrap();
        assert_eq!(reopened.views[0].columns, updated.views[0].columns);
        assert_eq!(reopened.views[1].columns, opened.views[1].columns);
        let parsed = parse(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(parsed["views"][0]["sort"], parse(yaml.as_bytes()).unwrap()["views"][0]["sort"]);
        assert!(set_columns(&root, &index, path, &opened.revision, 0, vec!["file.ext".into()]).is_err());
    }
}

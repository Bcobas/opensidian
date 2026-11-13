// SPDX-License-Identifier: GPL-3.0-or-later

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use yaml_rust2::{Yaml, YamlLoader};

use crate::{note_views, safe_rel, vaultfs::Vault, Index};

const MAX_BYTES: u64 = 128 * 1024;
const FILE_NAME: &str = "file.name";

#[derive(Deserialize)]
pub struct NewBase {
    pub columns: Vec<String>,
    pub sort: String,
    pub descending: bool,
    pub folder: Option<String>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    pub group_by: Option<GroupBy>,
}

#[derive(Deserialize, Serialize)]
pub struct GroupBy {
    property: String,
    direction: GroupDirection,
}

#[derive(Deserialize, Serialize)]
enum GroupDirection {
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
pub struct Table {
    title: String,
    columns: Vec<String>,
    sort: String,
    descending: bool,
    limit: Option<usize>,
    labels: BTreeMap<String, String>,
    rows: Vec<note_views::Row>,
    groups: Option<Vec<TableGroup>>,
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

fn base_path(path: &str) -> Result<PathBuf, String> {
    safe_rel(path).filter(|rel| is_base(rel)).ok_or_else(|| "Invalid .base path".into())
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
    if config.columns.is_empty() || config.columns.len() > 128 || !config.columns.iter().all(|key| column(key))
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

fn grouped_rows<'a>(
    view: &Yaml,
    docs: impl Iterator<Item = &'a (&'a str, note_views::Row, yaml_rust2::yaml::Hash)>,
) -> Result<(Vec<note_views::Row>, Option<Vec<TableGroup>>), String> {
    let by = &view["groupBy"];
    let order = &view["groupOrder"];
    if by.is_badvalue() {
        if !order.is_badvalue() { return Err("Group order requires a grouping property".into()); }
        return Ok((docs.map(|(_, row, _)| row.clone()).collect(), None));
    }
    let property = by["property"].as_str().ok_or("Invalid grouping property")?;
    let property = if property.contains('.') { property.to_owned() } else { format!("note.{property}") };
    if !column(&property) { return Err("Unsupported grouping property".into()); }
    let descending = match by["direction"].as_str() {
        Some("DESC") => true,
        Some("ASC") => false,
        None if by["direction"].is_badvalue() => false,
        _ => return Err("Unsupported group direction".into()),
    };
    let mut buckets = BTreeMap::<_, Vec<_>>::new();
    for (name, row, properties) in docs {
        buckets.entry(group_value(name, properties, &property)).or_default().push(row.clone());
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

pub fn open(root: &Path, index: &Index, path: &str) -> Result<Vec<Table>, String> {
    let path = base_path(path)?;
    let vault = Vault::open(root).map_err(|error| error.to_string())?;
    let bytes = vault.p_read(&path, MAX_BYTES).map_err(|error| error.to_string())?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "The base is not valid UTF-8")?;
    let docs = YamlLoader::load_from_str(text).map_err(|error| error.to_string())?;
    let [base] = docs.as_slice() else { return Err("The base must contain one YAML document".into()) };
    let global = Filter::parse(&base["filters"], 0)?;
    let views = base["views"].as_vec().filter(|views| !views.is_empty() && views.len() <= 16)
        .ok_or("The base must contain between one and sixteen views")?;
    let docs: Vec<_> = index.docs().filter_map(|(name, text, _)| {
        let properties = note_views::properties(text);
        global.matches(name, &properties).then(|| (name, note_views::row_from_properties(name, &properties), properties))
    }).collect();
    views.iter().filter(|view| view["type"].as_str() == Some("table")).map(|view| {
        if !view["summaries"].is_badvalue() {
            return Err("Summaries are not supported in base tables yet".into());
        }
        let mut columns = if view["order"].is_badvalue() {
            vec![FILE_NAME.to_owned()]
        } else {
            view["order"].as_vec().filter(|order| order.len() <= 128).ok_or("Invalid table column order")?
                .iter().map(|value| value.as_str().filter(|key| column(key)).map(str::to_owned)
                    .ok_or("This base uses an unsupported column")).collect::<Result<Vec<_>, _>>()?
        };
        if columns.is_empty() { columns.push(FILE_NAME.into()); }
        let (sort, descending) = if view["sort"].is_badvalue() {
            (columns[0].clone(), false)
        } else {
            let keys = view["sort"].as_vec().ok_or("Invalid base sort")?;
            match keys.as_slice() {
                [] => (columns[0].clone(), false),
                [key] => {
                    let name = key["property"].as_str().filter(|key| column(key)).ok_or("Unsupported base sort property")?;
                    let descending = match key["direction"].as_str() {
                        Some("DESC") => true, Some("ASC") => false,
                        _ => return Err("Unsupported base sort direction".into()),
                    };
                    (name.to_owned(), descending)
                }
                _ => return Err("Sorting by multiple properties is not supported yet".into()),
            }
        };
        let filter = Filter::parse(&view["filters"], 0)?;
        let matching = docs.iter().filter(|(name, _, properties)| filter.matches(name, properties));
        let (rows, groups) = grouped_rows(view, matching)?;
        let limit = view["limit"].as_i64().and_then(|limit| usize::try_from(limit).ok());
        let labels = columns.iter().filter_map(|key| {
            let props = &base["properties"];
            let label = props[key.as_str()]["displayName"].as_str()
                .or_else(|| key.strip_prefix("note.").and_then(|key| props[key]["displayName"].as_str()))?;
            Some((key.clone(), label.to_owned()))
        }).collect();
        Ok(Table { title: view["name"].as_str().unwrap_or("Notes").to_owned(), columns, sort, descending, limit, labels, rows, groups })
    }).collect::<Result<Vec<_>, _>>().and_then(|tables| {
        if tables.is_empty() { Err("This base has no supported table view".into()) } else { Ok(tables) }
    })
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
        let tables = open(&root, &index, "Project/Work.base").unwrap();
        assert_eq!(tables[0].columns, [FILE_NAME, "note.priority"]);
        assert_eq!(tables[0].sort, "note.priority");
        assert!(tables[0].descending);
        assert_eq!(tables[0].rows.len(), 2);
        index.upsert("Project/Sub/Alpha", "---\npriority: 7\n---\n");
        let tables = open(&root, &index, "Project/Work.base").unwrap();
        assert_eq!(serde_json::to_value(&tables[0].rows[1]).unwrap()["values"]["priority"], "7");
        create(&root, "Project/Scoped.base", NewBase { folder: Some("Project".into()), ..config() }).unwrap();
        assert_eq!(open(&root, &index, "Project/Scoped.base").unwrap()[0].rows.len(), 1);

        let yaml = "filters: 'file.inFolder(\"Project\")'\nviews:\n  - type: table\n    name: Project notes\n    order: [file.name, note.priority]\n    limit: 3\n";
        fs::write(root.join("Existing.base"), yaml).unwrap();
        let tables = open(&root, &index, "Existing.base").unwrap();
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
        let table = &open(&root, &index, "Filtered.base").unwrap()[0];
        assert_eq!(table.rows.len(), 1);
        assert_eq!(serde_json::to_value(&table.rows[0]).unwrap()["name"], "Project/Ready");
        index.upsert("Project/Ready", "---\npriority: 2\n---\n");
        assert!(open(&root, &index, "Filtered.base").unwrap()[0].rows.is_empty());

        create(&root, "Grouped.base", NewBase {
            group_by: Some(GroupBy { property: "note.priority".into(), direction: GroupDirection::Descending }),
            ..config()
        }).unwrap();
        let grouped = open(&root, &index, "Grouped.base").unwrap();
        assert!(grouped[0].rows.is_empty());
        let groups = grouped[0].groups.as_ref().unwrap();
        assert_eq!(groups.iter().map(|group| group.label.as_str()).collect::<Vec<_>>(),
            ["\"7\"", "9", "7", "2", "(No value)"]);
        index.upsert("Project/Tied", "---\npriority: 7.0\n---\n");
        let yaml = "views:\n  - type: table\n    filters: 'file.inFolder(\"Project\")'\n    groupBy: { property: priority, direction: ASC }\n    groupOrder: [null, 7, 7.0, Future]\n";
        fs::write(root.join("Manual.base"), yaml).unwrap();
        let manual = open(&root, &index, "Manual.base").unwrap();
        let groups = manual[0].groups.as_ref().unwrap();
        assert_eq!(groups.iter().map(|group| (group.label.as_str(), group.rows.len())).collect::<Vec<_>>(),
            [("(No value)", 0), ("7", 2), ("Future", 0)]);
        fs::write(root.join("Manual.base"), yaml.replace("[null, 7, 7.0, Future]", "[]")).unwrap();
        assert!(open(&root, &index, "Manual.base").unwrap()[0].groups.as_ref().unwrap().is_empty());
        fs::write(root.join("Manual.base"), yaml.replace("property: priority", "property: file.folder")
            .replace("    groupOrder: [null, 7, 7.0, Future]\n", "")).unwrap();
        let folders = open(&root, &index, "Manual.base").unwrap();
        assert_eq!(folders[0].groups.as_ref().unwrap().iter().map(|group| group.label.as_str()).collect::<Vec<_>>(),
            ["Project", "Project/Sub"]);
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
}

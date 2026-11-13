// SPDX-License-Identifier: GPL-3.0-or-later

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
}

#[derive(Serialize)]
pub struct Table {
    title: String,
    columns: Vec<String>,
    sort: String,
    descending: bool,
    limit: Option<usize>,
    labels: std::collections::BTreeMap<String, String>,
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
    let base = serde_json::json!({
        "filters": { "and": filters },
        "views": [{
            "type": "table", "name": "Notes", "order": config.columns,
            "sort": [{ "property": config.sort, "direction": if config.descending { "DESC" } else { "ASC" } }]
        }]
    });
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
}

impl Filter {
    fn parse(yaml: &Yaml, depth: usize) -> Result<Self, String> {
        let invalid = || "Unsupported base filter. Use file.ext or file.inFolder with and, or, or not.".to_owned();
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
        let comparison = expr.strip_prefix("file.ext").ok_or_else(invalid)?.trim_start();
        for (op, equal) in [("==", true), ("!=", false)] {
            if let Some(value) = comparison.strip_prefix(op) { return Ok(Self::Extension(parse_string(value)?, equal)); }
        }
        Err(invalid())
    }

    fn matches(&self, note: &str) -> bool {
        match self {
            Self::All(filters) => filters.iter().all(|filter| filter.matches(note)),
            Self::Any(filters) => filters.iter().any(|filter| filter.matches(note)),
            Self::None(filters) => !filters.iter().any(|filter| filter.matches(note)),
            Self::Extension(ext, equal) => (ext == "md") == *equal,
            Self::Folder(folder) => {
                let parent = note.rsplit_once('/').map_or("", |(parent, _)| parent);
                folder.is_empty() || parent == folder || parent.strip_prefix(folder).is_some_and(|s| s.starts_with('/'))
            }
        }
    }
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
    let docs: Vec<_> = index.docs().filter(|(name, _, _)| global.matches(name))
        .map(|(name, text, _)| (name, note_views::row(name, text))).collect();
    views.iter().filter(|view| view["type"].as_str() == Some("table")).map(|view| {
        if ["groupBy", "summaries"].iter().any(|key| !view[*key].is_badvalue()) {
            return Err("Grouping and summaries are not supported in base tables yet".into());
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
        let rows = docs.iter().filter(|(name, _)| filter.matches(name)).map(|(_, row)| row.clone()).collect();
        let limit = view["limit"].as_i64().and_then(|limit| usize::try_from(limit).ok());
        let labels = columns.iter().filter_map(|key| {
            let props = &base["properties"];
            let label = props[key.as_str()]["displayName"].as_str()
                .or_else(|| key.strip_prefix("note.").and_then(|key| props[key]["displayName"].as_str()))?;
            Some((key.clone(), label.to_owned()))
        }).collect();
        Ok(Table { title: view["name"].as_str().unwrap_or("Notes").to_owned(), columns, sort, descending, limit, labels, rows })
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
        let config = || NewBase { columns: vec![FILE_NAME.into(), "note.priority".into()], sort: "note.priority".into(), descending: true, folder: None };
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
    }
}

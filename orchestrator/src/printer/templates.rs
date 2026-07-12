//! Named custom-print templates — booth flyers, plaque inserts, anything the
//! operator reprints. A single JSON file living next to the config (same
//! convention as the crimes list), written via temp-file + rename.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::custom::PrintDoc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Template {
    pub name: String,
    pub doc: PrintDoc,
}

/// The list endpoint's row: just enough to populate a picker — full docs can
/// carry embedded images, so they're fetched one at a time.
#[derive(Debug, Clone, Serialize)]
pub struct TemplateMeta {
    pub name: String,
    pub blocks: usize,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TemplatesFile {
    templates: Vec<Template>,
}

pub struct TemplateStore {
    path: PathBuf,
    templates: Vec<Template>,
}

impl TemplateStore {
    /// A missing file is an empty store (templates are optional, unlike crimes).
    pub fn load_from_file(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let templates = match fs::read_to_string(&path) {
            Ok(text) => {
                let file: TemplatesFile = serde_json::from_str(&text)
                    .with_context(|| format!("parsing {}", path.display()))?;
                file.templates
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self { path, templates })
    }

    fn save(&self) -> Result<()> {
        let file = TemplatesFile { templates: self.templates.clone() };
        let mut text = serde_json::to_string_pretty(&file).context("serializing templates")?;
        text.push('\n');
        // Temp file + rename so a crash mid-write can't truncate the only copy.
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("renaming {} into place", tmp.display()))?;
        Ok(())
    }

    pub fn list(&self) -> Vec<TemplateMeta> {
        self.templates
            .iter()
            .map(|t| TemplateMeta { name: t.name.clone(), blocks: t.doc.blocks.len() })
            .collect()
    }

    pub fn get(&self, name: &str) -> Option<&PrintDoc> {
        self.templates.iter().find(|t| t.name == name).map(|t| &t.doc)
    }

    /// Upsert by name and persist.
    pub fn put(&mut self, name: &str, doc: PrintDoc) -> Result<()> {
        validate_name(name)?;
        match self.templates.iter_mut().find(|t| t.name == name) {
            Some(t) => t.doc = doc,
            None => self.templates.push(Template { name: name.to_string(), doc }),
        }
        self.save()
    }

    /// Remove by name and persist. Returns false if no such template.
    pub fn remove(&mut self, name: &str) -> Result<bool> {
        let before = self.templates.len();
        self.templates.retain(|t| t.name != name);
        if self.templates.len() == before {
            return Ok(false);
        }
        self.save()?;
        Ok(true)
    }
}

/// Names travel as URL path segments and file content — keep them tame.
pub fn validate_name(name: &str) -> Result<()> {
    anyhow::ensure!(
        !name.is_empty() && name.len() <= 60,
        "template name must be 1-60 chars"
    );
    anyhow::ensure!(
        name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-')),
        "template name allows letters, digits, space, . _ -"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::printer::custom::Block;

    fn doc() -> PrintDoc {
        PrintDoc {
            blocks: vec![Block::Rule { heavy: false }],
            length_mm: Some(50.0),
        }
    }

    #[test]
    fn missing_file_is_empty_store() {
        let store = TemplateStore::load_from_file("/nonexistent/dir/tpl.json.notthere").unwrap();
        assert!(store.list().is_empty());
    }

    #[test]
    fn put_get_remove_roundtrip_persists() {
        let dir = std::env::temp_dir().join(format!("wetcourt_tpl_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("print_templates.json");
        let _ = std::fs::remove_file(&path);

        let mut store = TemplateStore::load_from_file(&path).unwrap();
        store.put("plaque 50mm", doc()).unwrap();
        assert_eq!(store.list().len(), 1);
        assert_eq!(store.get("plaque 50mm").unwrap().length_mm, Some(50.0));

        // Reload from disk: survives restart, no stray temp file.
        let reloaded = TemplateStore::load_from_file(&path).unwrap();
        assert_eq!(reloaded.list().len(), 1);
        assert!(!path.with_extension("json.tmp").exists());

        let mut store = reloaded;
        assert!(store.remove("plaque 50mm").unwrap());
        assert!(!store.remove("plaque 50mm").unwrap());
        assert!(TemplateStore::load_from_file(&path).unwrap().list().is_empty());
    }

    #[test]
    fn names_are_validated() {
        let mut store = TemplateStore::load_from_file("/tmp/unused_tpl.json").unwrap();
        assert!(store.put("", doc()).is_err());
        assert!(store.put("../escape", doc()).is_err());
        assert!(store.put(&"x".repeat(61), doc()).is_err());
    }
}

//! Client configuration (SPEC §12.1): machine-local, non-semantic
//! settings in ~/.cuj/config.toml. Nothing here may affect the
//! meaning of vault contents.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub vault: PathBuf,
    pub active_profile: String,
    pub editor: Option<String>,
    /// Extraction tool map: lowercase extension -> command template
    /// with `{in}` / `{out}` placeholders (SPEC §7.2).
    pub extract: BTreeMap<String, String>,
    /// Space name -> local directory (SPEC §space registry);
    /// machine-local by design.
    pub spaces: BTreeMap<String, PathBuf>,
    /// Path of the config file itself (for `config set`).
    pub path: PathBuf,
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/".into()))
}

fn default_vault() -> PathBuf {
    match std::env::var_os("XDG_DATA_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x).join("cuj/vault"),
        _ => home().join(".local/share/cuj/vault"),
    }
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig {
            vault: default_vault(),
            active_profile: "default".to_string(),
            editor: None,
            extract: BTreeMap::from([("pdf".to_string(), "pdftotext {in} {out}".to_string())]),
            spaces: BTreeMap::new(),
            path: home().join(".cuj/config.toml"),
        }
    }
}

impl ClientConfig {
    pub fn load() -> Result<Self> {
        let mut cfg = ClientConfig::default();
        if let Some(p) = std::env::var_os("CUJ_CONFIG") {
            cfg.path = PathBuf::from(p);
        }
        let text = match std::fs::read_to_string(&cfg.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(cfg),
            Err(e) => return Err(e.into()),
        };
        let doc: toml::Value = text
            .parse()
            .map_err(|e| Error::Substrate(format!("config.toml: {e}")))?;
        if let Some(v) = doc.get("vault").and_then(|v| v.as_str()) {
            cfg.vault = expand_tilde(v);
        }
        if let Some(v) = doc.get("active_profile").and_then(|v| v.as_str()) {
            cfg.active_profile = v.to_string();
        }
        if let Some(v) = doc.get("editor").and_then(|v| v.as_str()) {
            cfg.editor = Some(v.to_string());
        }
        if let Some(t) = doc.get("extract").and_then(|v| v.as_table()) {
            cfg.extract.clear();
            for (k, v) in t {
                if let Some(cmd) = v.as_str() {
                    cfg.extract.insert(k.to_lowercase(), cmd.to_string());
                }
            }
        }
        if let Some(t) = doc.get("space").and_then(|v| v.as_table()) {
            for (k, v) in t {
                if let Some(dir) = v.as_str() {
                    cfg.spaces.insert(k.to_lowercase(), expand_tilde(dir));
                }
            }
        }
        Ok(cfg)
    }

    /// Rewrite the config file preserving only known keys (the file
    /// is small and cuj-owned; unknown keys are preserved verbatim
    /// by re-emitting the parsed document with ours updated).
    pub fn save(&self) -> Result<()> {
        let mut doc: toml::value::Table = match std::fs::read_to_string(&self.path) {
            // An unparseable file is refused, not overwritten —
            // clobbering it would silently drop the unknown keys
            // this function promises to preserve.
            Ok(t) => t
                .parse::<toml::Value>()
                .map_err(|e| {
                    Error::Substrate(format!(
                        "config.toml: {e} (fix or remove it; refusing to overwrite)"
                    ))
                })?
                .as_table()
                .cloned()
                .unwrap_or_default(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
            Err(e) => return Err(e.into()),
        };
        let had_extract = doc.contains_key("extract");
        doc.insert(
            "vault".into(),
            toml::Value::String(self.vault.to_string_lossy().into_owned()),
        );
        doc.insert(
            "active_profile".into(),
            toml::Value::String(self.active_profile.clone()),
        );
        match &self.editor {
            Some(e) => {
                doc.insert("editor".into(), toml::Value::String(e.clone()));
            }
            None => {
                doc.remove("editor");
            }
        }
        // Only materialize [extract] when the user has one (or has
        // diverged from the built-in default) — a config that never
        // mentioned it should stay that way.
        if had_extract || self.extract != ClientConfig::default().extract {
            let mut extract = toml::value::Table::new();
            for (k, v) in &self.extract {
                extract.insert(k.clone(), toml::Value::String(v.clone()));
            }
            doc.insert("extract".into(), toml::Value::Table(extract));
        }
        let mut spaces = toml::value::Table::new();
        for (k, v) in &self.spaces {
            spaces.insert(
                k.clone(),
                toml::Value::String(v.to_string_lossy().into_owned()),
            );
        }
        if spaces.is_empty() {
            doc.remove("space");
        } else {
            doc.insert("space".into(), toml::Value::Table(spaces));
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let out = toml::to_string_pretty(&toml::Value::Table(doc))
            .map_err(|e| Error::Substrate(format!("config.toml: {e}")))?;
        // Write-then-rename: a crash mid-write must not truncate
        // the config.
        let dir = self.path.parent().unwrap_or(std::path::Path::new("."));
        let tmp = tempfile::NamedTempFile::new_in(dir)?;
        std::fs::write(tmp.path(), &out)?;
        tmp.persist(&self.path).map_err(|e| Error::from(e.error))?;
        Ok(())
    }

    pub fn editor_command(&self) -> String {
        self.editor
            .clone()
            .or_else(|| std::env::var("VISUAL").ok().filter(|s| !s.is_empty()))
            .or_else(|| std::env::var("EDITOR").ok().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| "vi".to_string())
    }
}

fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        home().join(rest)
    } else {
        PathBuf::from(p)
    }
}

use std::path::{Path, PathBuf};

use ignore::{WalkBuilder, overrides::OverrideBuilder};

use crate::model::DocFormat;

#[derive(Debug, Default, Clone)]
pub struct WalkOptions {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

pub fn detect_format(path: &Path) -> Option<DocFormat> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "md" | "markdown" => Some(DocFormat::Markdown),
        "htm" | "html" => Some(DocFormat::Html),
        "txt" | "text" => Some(DocFormat::Text),
        _ => None,
    }
}

pub fn walk(paths: &[PathBuf], options: &WalkOptions) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_file() {
            if detect_format(path).is_some() {
                files.push(path.clone());
            }
            continue;
        }
        let mut overrides = OverrideBuilder::new(path);
        for glob in &options.include {
            overrides.add(glob).map_err(|e| e.to_string())?;
        }
        for glob in &options.exclude {
            overrides
                .add(&format!("!{glob}"))
                .map_err(|e| e.to_string())?;
        }
        let overrides = overrides.build().map_err(|e| e.to_string())?;
        let mut builder = WalkBuilder::new(path);
        builder.hidden(false).overrides(overrides);
        for entry in builder.build() {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_type().is_some_and(|kind| kind.is_file())
                && detect_format(entry.path()).is_some()
            {
                files.push(entry.into_path());
            }
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

use std::path::{Path, PathBuf};

use ignore::{WalkBuilder, overrides::OverrideBuilder};

use crate::model::DocFormat;

#[derive(Debug, Default, Clone)]
pub struct WalkOptions {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

pub fn detect_format(path: &Path, bytes: &[u8]) -> DocFormat {
    if bytes.starts_with(b"%PDF-") {
        return DocFormat::Pdf;
    }
    if bytes.starts_with(b"PK\x03\x04")
        && let Ok(archive) = zip::ZipArchive::new(std::io::Cursor::new(bytes))
    {
        if archive.file_names().any(|name| name.starts_with("word/")) {
            return DocFormat::Docx;
        }
        if archive.file_names().any(|name| name.starts_with("xl/")) {
            return DocFormat::Xlsx;
        }
        if archive.file_names().any(|name| name.starts_with("ppt/")) {
            return DocFormat::Pptx;
        }
    }
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("md" | "markdown") => Some(DocFormat::Markdown),
        Some("htm" | "html") => Some(DocFormat::Html),
        Some("txt" | "text") => Some(DocFormat::Text),
        Some("pdf") => Some(DocFormat::Pdf),
        Some("docx") => Some(DocFormat::Docx),
        Some("xlsx") => Some(DocFormat::Xlsx),
        Some("pptx") => Some(DocFormat::Pptx),
        _ => Some(DocFormat::Text),
    }
    .unwrap_or(DocFormat::Text)
}

pub fn walk(paths: &[PathBuf], options: &WalkOptions) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_file() {
            files.push(path.clone());
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
            if entry.file_type().is_some_and(|kind| kind.is_file()) {
                files.push(entry.into_path());
            }
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_bytes_override_extensions_and_extensionless_files() {
        assert_eq!(
            detect_format(Path::new("wrong.txt"), b"%PDF-1.4"),
            DocFormat::Pdf
        );
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file(
            "word/document.xml",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        assert_eq!(
            detect_format(Path::new("no-extension"), &bytes),
            DocFormat::Docx
        );
    }
}

//! Filesystem application of suggested fixes: in-place writes with backup and
//! restore-on-verify-failure, fixed-copy writes, and preflight refusals for
//! unmodifiable PDFs.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use tempfile::NamedTempFile;

use crate::{
    extract::{SourceDocument, extract},
    fix::pdf_refusal,
    model::{DocFormat, Finding},
    walk::detect_format,
};

/// `<name>.fixed.<ext>` sibling path used by copy mode.
pub fn fixed_copy_path(path: &Path) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    match path.extension() {
        Some(extension) => {
            path.with_file_name(format!("{stem}.fixed.{}", extension.to_string_lossy()))
        }
        None => path.with_file_name(format!("{stem}.fixed")),
    }
}

/// Write `bytes` to a new `path`, refusing to overwrite an existing file.
pub fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if path.exists() {
        return Err("refusing to overwrite existing fixed copy".into());
    }
    fs::write(path, bytes).map_err(|error| format!("writing copy: {error}"))
}

/// Atomically replace `path` with `fixed`, preserving permissions, optionally
/// writing a `.bak` backup first, and restoring the original when `verify`
/// rejects the written content.
pub fn write_in_place(
    path: &Path,
    original: &[u8],
    fixed: &[u8],
    backup: bool,
    verify: impl FnOnce(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    if backup {
        fs::write(
            path.with_extension(format!(
                "{}bak",
                path.extension()
                    .map_or_else(String::new, |ext| format!("{}.", ext.to_string_lossy()))
            )),
            original,
        )
        .map_err(|error| format!("writing backup: {error}"))?;
    }
    let metadata = fs::metadata(path).map_err(|error| format!("reading metadata: {error}"))?;
    let original_permissions = metadata.permissions();
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = NamedTempFile::new_in(directory)
        .map_err(|error| format!("creating temporary file: {error}"))?;
    temporary
        .write_all(fixed)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| format!("writing temporary file: {error}"))?;
    temporary
        .as_file()
        .set_permissions(original_permissions.clone())
        .map_err(|error| format!("preserving permissions: {error}"))?;
    #[cfg(windows)]
    {
        // MoveFileEx cannot replace a read-only destination.
        let mut writable = original_permissions.clone();
        #[allow(clippy::permissions_set_readonly_false)]
        writable.set_readonly(false);
        fs::set_permissions(path, writable)
            .map_err(|error| format!("making original replaceable: {error}"))?;
    }
    temporary.persist(path).map_err(|error| {
        #[cfg(windows)]
        let _ = fs::set_permissions(path, original_permissions.clone());
        format!("replacing original: {}", error.error)
    })?;
    fs::set_permissions(path, original_permissions.clone()).map_err(|error| {
        restore_original(path, original, &original_permissions)
            .err()
            .map_or_else(
                || format!("restoring permissions after replacement: {error}"),
                |restore| format!("restoring permissions after replacement: {error}; {restore}"),
            )
    })?;
    let result = fs::read(path)
        .map_err(|error| format!("reading written file: {error}"))
        .and_then(|written| verify(&written));
    if let Err(error) = result {
        restore_original(path, original, &original_permissions)
            .map_err(|restore| format!("{error}; {restore}"))?;
        return Err(error);
    }
    Ok(())
}

fn restore_original(
    path: &Path,
    original: &[u8],
    permissions: &fs::Permissions,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        let mut writable = permissions.clone();
        #[allow(clippy::permissions_set_readonly_false)]
        writable.set_readonly(false);
        fs::set_permissions(path, writable)
            .map_err(|error| format!("making original writable: {error}"))?;
    }
    fs::write(path, original).map_err(|error| format!("restoring original bytes: {error}"))?;
    fs::set_permissions(path, permissions.clone())
        .map_err(|error| format!("restoring original permissions: {error}"))
}

/// Re-extract links from written `bytes` and confirm every suggested
/// replacement is present and no original URL survives.
pub fn verify_fixed(
    path: &Path,
    format: DocFormat,
    bytes: &[u8],
    findings: &[Finding],
) -> Result<(), String> {
    let links = extract(&SourceDocument {
        path: path.to_path_buf(),
        format,
        bytes: bytes.to_vec(),
    })
    .map_err(|error| format!("re-parsing fixed file: {}", error.0))?;
    for finding in findings {
        let replacement = &finding
            .fix
            .as_ref()
            .expect("selected finding has a fix")
            .replacement_url;
        if !links.iter().any(|link| link.url == *replacement) {
            return Err(format!(
                "replacement URL was not extractable: {replacement}"
            ));
        }
        if links.iter().any(|link| link.url == finding.url) {
            return Err(format!("old URL is still extractable: {}", finding.url));
        }
    }
    Ok(())
}

/// Return every PDF in `paths` that refuses automatic fixing, paired with the
/// refusal message. Callers print the messages and skip the returned paths.
pub fn preflight_pdfs(paths: &[PathBuf]) -> Vec<(PathBuf, String)> {
    let mut refused = Vec::new();
    for path in paths {
        let Ok(bytes) = fs::read(path) else { continue };
        if detect_format(path, &bytes) != Some(DocFormat::Pdf) {
            continue;
        }
        let result = lopdf::Document::load_mem(&bytes)
            .map_err(|error| format!("reading PDF: {error}"))
            .and_then(|document| pdf_refusal(&document).map_err(|error| error.0));
        if let Err(error) = result {
            refused.push((path.clone(), error));
        }
    }
    refused
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_verification_restores_original_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("note.txt");
        let original = b"original\0bytes";
        fs::write(&path, original).unwrap();

        let result = write_in_place(&path, original, b"fixed", false, |_| {
            Err("failed verification".into())
        });

        assert_eq!(result.unwrap_err(), "failed verification");
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn failed_verification_restores_binary_document_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fixture.docx");
        let original = b"PK\x03\x04binary OOXML bytes\0\xff";
        fs::write(&path, original).unwrap();

        let result = write_in_place(
            &path,
            original,
            b"PK\x03\x04fixed OOXML bytes",
            false,
            |_| Err("failed verification".into()),
        );

        assert_eq!(result.unwrap_err(), "failed verification");
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn failed_verification_restores_pdf_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fixture.pdf");
        let original = b"%PDF-1.4\noriginal PDF bytes\0\xff\n%%EOF\n";
        fs::write(&path, original).unwrap();

        let result = write_in_place(
            &path,
            original,
            b"%PDF-1.4\nfixed PDF bytes\n%%EOF\n",
            false,
            |_| Err("failed verification".into()),
        );

        assert_eq!(result.unwrap_err(), "failed verification");
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[cfg(windows)]
    #[test]
    fn in_place_write_preserves_windows_readonly_attribute() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("readonly.txt");
        fs::write(&path, b"original").unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).unwrap();

        write_in_place(&path, b"original", b"fixed", false, |_| Ok(())).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"fixed");
        assert!(fs::metadata(path).unwrap().permissions().readonly());
    }

    #[cfg(windows)]
    #[test]
    fn failed_verification_restores_windows_readonly_attribute() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("readonly.txt");
        fs::write(&path, b"original").unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).unwrap();

        let result = write_in_place(&path, b"original", b"fixed", false, |_| {
            Err("failed verification".into())
        });

        assert_eq!(result.unwrap_err(), "failed verification");
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert!(fs::metadata(&path).unwrap().permissions().readonly());
    }

    #[cfg(unix)]
    #[test]
    fn in_place_write_preserves_unix_mode() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("readonly.txt");
        fs::write(&path, b"original").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();

        write_in_place(&path, b"original", b"fixed", false, |_| Ok(())).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"fixed");
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o444
        );
    }

    #[test]
    fn write_new_file_refuses_existing_destination() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("report.fixed.md");
        fs::write(&path, b"taken").unwrap();

        let result = write_new_file(&path, b"new");

        assert_eq!(
            result.unwrap_err(),
            "refusing to overwrite existing fixed copy"
        );
        assert_eq!(fs::read(&path).unwrap(), b"taken");
    }

    #[test]
    fn write_new_file_writes_absent_destination() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("report.fixed.md");

        write_new_file(&path, b"new").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn fixed_copy_path_preserves_extension() {
        assert_eq!(
            fixed_copy_path(Path::new("/docs/report.md")),
            Path::new("/docs/report.fixed.md")
        );
        assert_eq!(
            fixed_copy_path(Path::new("/docs/report")),
            Path::new("/docs/report.fixed")
        );
    }

    #[test]
    fn preflight_pdfs_refuses_unparseable_pdf() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("broken.pdf");
        fs::write(&path, b"not a real pdf").unwrap();

        let refused = preflight_pdfs(std::slice::from_ref(&path));

        assert_eq!(refused.len(), 1);
        assert_eq!(refused[0].0, path);
        assert!(refused[0].1.starts_with("reading PDF:"));
    }

    #[test]
    fn preflight_pdfs_skips_non_pdf_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("note.txt");
        fs::write(&path, b"plain text").unwrap();

        assert!(preflight_pdfs(&[path]).is_empty());
    }
}

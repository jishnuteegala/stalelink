use std::{
    fs,
    io::{Cursor, Write},
};

use predicates::prelude::PredicateBooleanExt;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

mod util;

const OLD_PATH: &str = "/old";

fn archive(entries: &[(&str, String)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, contents) in entries {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(contents.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn ooxml(format: &str, old: &str) -> Vec<u8> {
    let rel = format!(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="hyperlink" Target="{old}" TargetMode="External"/></Relationships>"#
    );
    let entries = match format {
        "docx" => vec![
            ("word/document.xml", r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:hyperlink r:id="rId1"><w:r><w:t>link</w:t></w:r></w:hyperlink></w:p></w:body></w:document>"#.into()),
            ("word/_rels/document.xml.rels", rel),
            ("word/media/keep.bin", "untouched compressed payload".into()),
        ],
        "xlsx" => vec![
            ("xl/workbook.xml", r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#.into()),
            ("xl/_rels/workbook.xml.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Target="worksheets/sheet1.xml"/></Relationships>"#.into()),
            ("xl/worksheets/sheet1.xml", r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><hyperlinks><hyperlink ref="A1" r:id="rId1"/></hyperlinks></worksheet>"#.into()),
            ("xl/worksheets/_rels/sheet1.xml.rels", rel),
            ("xl/media/keep.bin", "untouched compressed payload".into()),
        ],
        "pptx" => vec![
            ("ppt/slides/slide1.xml", r#"<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:rPr><a:hlinkClick r:id="rId1"/></a:rPr></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#.into()),
            ("ppt/slides/_rels/slide1.xml.rels", rel),
            ("ppt/media/keep.bin", "untouched compressed payload".into()),
        ],
        _ => unreachable!(),
    };
    archive(&entries)
}

fn pdf(old: &str, bare_text: bool, trailer_extra: &str, catalog_extra: &str) -> Vec<u8> {
    let contents = if bare_text {
        let text = format!("BT /F1 12 Tf 72 720 Td ({old}) Tj ET");
        format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len())
    } else {
        "<< /Length 0 >>\nstream\n\nendstream".into()
    };
    let mut objects = vec![
        format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 6 0 R >> >> /Contents 4 0 R /Annots [5 0 R] >>".into(),
        contents,
        format!("<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] /A << /S /URI /URI ({old}) >> >>"),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
    ];
    if trailer_extra.contains("Encrypt") {
        objects.push("<< /Filter /Standard >>".into());
    }
    let mut output = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0];
    for (number, object) in objects.iter().enumerate() {
        offsets.push(output.len());
        output.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", number + 1).as_bytes());
    }
    let xref = output.len();
    output.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets.iter().skip(1) {
        output.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    output.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R {trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    output
}

fn raw_entry(bytes: &[u8], name: &str) -> Vec<u8> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let entry = archive.by_name(name).unwrap();
    bytes[entry.data_start() as usize..(entry.data_start() + entry.compressed_size()) as usize]
        .to_vec()
}

async fn redirect_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(path("/old"))
        .respond_with(
            ResponseTemplate::new(301)
                .insert_header("location", format!("{}/new?x=1&y=2", server.uri())),
        )
        .mount(&server)
        .await;
    Mock::given(path("/new"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(path("/a(b)"))
        .respond_with(
            ResponseTemplate::new(301).insert_header("location", format!("{}/new", server.uri())),
        )
        .mount(&server)
        .await;
    server
}

fn fix(path: &std::path::Path, args: &[&str]) -> assert_cmd::assert::Assert {
    let mut command = util::command();
    command.args(["fix", "--no-cache", "--retries", "0"]);
    command.args(args);
    command.arg(path);
    tokio::task::block_in_place(|| command.assert())
}

#[tokio::test(flavor = "multi_thread")]
async fn dry_run_prints_diff_and_does_not_modify_text_files() {
    let server = redirect_server().await;
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("note.md");
    let original = format!("before\n[old]({}/old)\nafter\n", server.uri());
    fs::write(&file, &original).unwrap();

    let output = fix(&file, &["--min-fix-confidence", "outdated"])
        .code(0)
        .stderr("")
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("--- a/"));
    assert!(stdout.contains(&format!("-[old]({}/old)", server.uri())));
    assert!(stdout.contains(&format!("+[old]({}/new?x=1&y=2)", server.uri())));
    assert_eq!(fs::read_to_string(&file).unwrap(), original);
}

#[tokio::test(flavor = "multi_thread")]
async fn write_updates_markdown_text_and_html_without_changing_other_bytes() {
    let server = redirect_server().await;
    let directory = tempfile::tempdir().unwrap();
    let markdown = directory.path().join("note.md");
    let text = directory.path().join("note.txt");
    let html = directory.path().join("page.html");
    fs::write(
        &markdown,
        format!("prefix [x]({}/old) suffix\n", server.uri()),
    )
    .unwrap();
    fs::write(&text, format!("prefix {}/old suffix\n", server.uri())).unwrap();
    fs::write(
        &html,
        format!(
            r#"<a href="{0}/old">x</a><img src="{0}/old">"#,
            server.uri()
        ),
    )
    .unwrap();

    fix(
        directory.path(),
        &["--write", "--min-fix-confidence", "outdated"],
    )
    .code(0)
    .stderr("");
    for file in [&markdown, &text, &html] {
        let actual = fs::read_to_string(file).unwrap();
        assert!(!actual.contains("/old"));
        assert!(actual.contains(&format!("{}/new", server.uri())));
    }
    assert_eq!(
        fs::read_to_string(&markdown).unwrap(),
        format!("prefix [x]({}/new?x=1&y=2) suffix\n", server.uri())
    );
    assert_eq!(
        fs::read_to_string(&text).unwrap(),
        format!("prefix {}/new?x=1&y=2 suffix\n", server.uri())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn backup_and_copy_have_their_documented_effects() {
    let server = redirect_server().await;
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("note.txt");
    let original = format!("{}/old\n", server.uri());
    fs::write(&file, &original).unwrap();

    fix(
        &file,
        &["--write", "--backup", "--min-fix-confidence", "outdated"],
    )
    .code(0)
    .stderr("");
    assert_eq!(
        fs::read_to_string(file.with_extension("txt.bak")).unwrap(),
        original
    );
    assert!(fs::read_to_string(&file).unwrap().contains("/new"));

    fs::write(&file, &original).unwrap();
    fix(&file, &["--copy", "--min-fix-confidence", "outdated"])
        .code(0)
        .stderr("");
    assert_eq!(fs::read_to_string(&file).unwrap(), original);
    assert!(
        fs::read_to_string(directory.path().join("note.fixed.txt"))
            .unwrap()
            .contains("/new")
    );
}

#[test]
fn usage_rejects_copy_and_write_together() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("note.txt"), "https://example.test\n").unwrap();
    fix(directory.path(), &["--copy", "--write"]).code(2);
}

#[tokio::test(flavor = "multi_thread")]
async fn write_refuses_escaped_markdown_destinations_without_changing_bytes() {
    let server = redirect_server().await;
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("escaped.md");
    let original = format!("[escaped]({}/a\\(b\\))\n", server.uri());
    fs::write(&file, &original).unwrap();

    fix(&file, &["--write", "--min-fix-confidence", "outdated"])
        .code(1)
        .stderr(predicates::str::contains(
            "source bytes are not the semantic URL",
        ));
    assert_eq!(fs::read_to_string(file).unwrap(), original);
}

#[tokio::test(flavor = "multi_thread")]
async fn write_preserves_markdown_and_html_syntax_outside_url_values() {
    let server = redirect_server().await;
    let directory = tempfile::tempdir().unwrap();
    let markdown = directory.path().join("links.md");
    let html = directory.path().join("page.html");
    let old = format!("{}/old", server.uri());
    let new = format!("{}/new?x=1&amp;y=2", server.uri());
    let markdown_new = format!("{}/new?x=1&y=2", server.uri());
    let markdown_original = format!(
        "before\r\n[inline](<{old}>)\r\n[ref][r]\r\n[r]: <{old}> \\\"title\\\"\r\n<{}>\r\nafter\r\n",
        old
    );
    let html_original = format!(
        "<A HREF = '{old}?x=1&amp;y=2' data-x = \"keep\"><img SRC={old}?x=1&#38;y=2><link href=\"{old}?x=1&amp;y=2\"></A>"
    );
    fs::write(&markdown, &markdown_original).unwrap();
    fs::write(&html, &html_original).unwrap();

    fix(
        directory.path(),
        &["--write", "--min-fix-confidence", "outdated"],
    )
    .code(0)
    .stderr("");

    assert_eq!(
        fs::read_to_string(&markdown).unwrap(),
        format!(
            "before\r\n[inline](<{}>)\r\n[ref][r]\r\n[r]: <{}> \\\"title\\\"\r\n<{}>\r\nafter\r\n",
            markdown_new, markdown_new, markdown_new,
        )
    );
    assert_eq!(
        fs::read_to_string(&html).unwrap(),
        format!(
            "<A HREF = '{new}' data-x = \"keep\"><img SRC={}/new?x&#61;1&amp;y&#61;2><link href=\"{new}\"></A>",
            server.uri()
        )
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn write_fixes_html_c1_numeric_character_references() {
    let server = redirect_server().await;
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("page.html");
    fs::write(
        &file,
        format!(r#"<a href='{}/old?currency=&#128;'>x</a>"#, server.uri()),
    )
    .unwrap();

    fix(&file, &["--write", "--min-fix-confidence", "outdated"])
        .code(0)
        .stderr("");

    assert_eq!(
        fs::read_to_string(file).unwrap(),
        format!(r#"<a href='{}/new?x=1&amp;y=2'>x</a>"#, server.uri())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn write_fixes_each_ooxml_format_and_raw_copies_untouched_entries() {
    let server = redirect_server().await;
    let old = format!("{}{OLD_PATH}", server.uri());
    let replacement = format!("{}/new?x=1&y=2", server.uri());
    let directory = tempfile::tempdir().unwrap();

    for (extension, untouched) in [
        ("docx", "word/media/keep.bin"),
        ("xlsx", "xl/media/keep.bin"),
        ("pptx", "ppt/media/keep.bin"),
    ] {
        let file = directory.path().join(format!("fixture.{extension}"));
        let original = ooxml(extension, &old);
        fs::write(&file, &original).unwrap();

        fix(&file, &["--write", "--min-fix-confidence", "outdated"])
            .code(0)
            .stderr("");

        let fixed = fs::read(&file).unwrap();
        let format = match extension {
            "docx" => stalelink_core::model::DocFormat::Docx,
            "xlsx" => stalelink_core::model::DocFormat::Xlsx,
            "pptx" => stalelink_core::model::DocFormat::Pptx,
            _ => unreachable!(),
        };
        let links = stalelink_core::extract::extract(&stalelink_core::extract::SourceDocument {
            path: file.clone(),
            format,
            bytes: fixed.clone(),
        })
        .unwrap();
        assert_eq!(links.len(), 1, "{extension}");
        assert_eq!(links[0].url, replacement, "{extension}");
        assert_eq!(
            raw_entry(&original, untouched),
            raw_entry(&fixed, untouched),
            "{extension} changed an untouched ZIP member"
        );
        let before = zip::ZipArchive::new(Cursor::new(original)).unwrap();
        let after = zip::ZipArchive::new(Cursor::new(fixed)).unwrap();
        assert_eq!(before.len(), after.len(), "{extension}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn binary_dry_run_prints_summary_without_writing() {
    let server = redirect_server().await;
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("fixture.docx");
    let original = ooxml("docx", &format!("{}{OLD_PATH}", server.uri()));
    fs::write(&file, &original).unwrap();

    let output = fix(&file, &["--min-fix-confidence", "outdated"])
        .code(0)
        .stderr("")
        .get_output()
        .clone();
    assert_eq!(fs::read(&file).unwrap(), original);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "{}: {}/old -> {}/new?x=1&y=2\n",
            file.display(),
            server.uri(),
            server.uri()
        )
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn write_fixes_pdf_annotation_incrementally() {
    let server = redirect_server().await;
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("fixture.pdf");
    let original = pdf(&format!("{}{OLD_PATH}", server.uri()), false, "", "");
    let original_objects = lopdf::Document::load_mem(&original).unwrap().objects.len();
    fs::write(&file, &original).unwrap();

    fix(&file, &["--write", "--min-fix-confidence", "outdated"])
        .code(0)
        .stderr("");

    let fixed = fs::read(&file).unwrap();
    assert!(fixed.starts_with(&original));
    assert_eq!(
        fixed
            .windows(5)
            .filter(|window| *window == b"%%EOF")
            .count(),
        2
    );
    assert_eq!(
        lopdf::Document::load_mem(&fixed).unwrap().objects.len(),
        original_objects
    );
    let links = stalelink_core::extract::extract(&stalelink_core::extract::SourceDocument {
        path: file,
        format: stalelink_core::model::DocFormat::Pdf,
        bytes: fixed,
    })
    .unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].url, format!("{}/new?x=1&y=2", server.uri()));
}

#[tokio::test(flavor = "multi_thread")]
async fn encrypted_and_signed_pdfs_are_refused() {
    let server = redirect_server().await;
    let directory = tempfile::tempdir().unwrap();
    let old = format!("{}{OLD_PATH}", server.uri());
    let encrypted = directory.path().join("encrypted.pdf");
    let signed = directory.path().join("signed.pdf");
    let encrypted_bytes = pdf(&old, false, "/Encrypt 7 0 R", "");
    let signed_bytes = pdf(&old, false, "", "/Perms <<>>");
    assert!(
        lopdf::Document::load_mem(&encrypted_bytes)
            .unwrap()
            .is_encrypted()
    );
    fs::write(&encrypted, &encrypted_bytes).unwrap();
    fs::write(&signed, &signed_bytes).unwrap();

    fix(&encrypted, &["--write", "--min-fix-confidence", "outdated"])
        .code(1)
        .stderr(predicates::str::contains(
            "encrypted PDF files are not modified",
        ));
    assert_eq!(fs::read(&encrypted).unwrap(), encrypted_bytes);
    fix(&signed, &["--write", "--min-fix-confidence", "outdated"])
        .code(1)
        .stderr(predicates::str::contains(
            "signed PDF files are not modified",
        ));
    assert_eq!(fs::read(&signed).unwrap(), signed_bytes);
}

#[tokio::test(flavor = "multi_thread")]
async fn bare_pdf_text_url_is_refused_and_pdf_exclusion_skips_it() {
    let server = redirect_server().await;
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("bare.pdf");
    let original = pdf(&format!("{}{OLD_PATH}", server.uri()), true, "", "");
    fs::write(&file, &original).unwrap();

    fix(&file, &["--write", "--min-fix-confidence", "outdated"])
        .code(1)
        .stderr(predicates::str::contains(
            "bare PDF text URLs require manual editing",
        ))
        .stderr(predicates::str::contains("encrypted PDF").not())
        .stderr(predicates::str::contains("signed PDF").not());
    assert_eq!(fs::read(&file).unwrap(), original);
    fix(
        &file,
        &[
            "--write",
            "--fix-exclude",
            "pdf",
            "--min-fix-confidence",
            "outdated",
        ],
    )
    .code(0)
    .stderr("");
    assert_eq!(fs::read(&file).unwrap(), original);
}

#[tokio::test(flavor = "multi_thread")]
async fn pdf_exclusion_does_not_skip_text_fixes() {
    let server = redirect_server().await;
    let directory = tempfile::tempdir().unwrap();
    let text = directory.path().join("note.txt");
    let pdf_file = directory.path().join("note.pdf");
    let old = format!("{}{OLD_PATH}", server.uri());
    fs::write(&text, &old).unwrap();
    let original_pdf = pdf(&old, true, "", "");
    fs::write(&pdf_file, &original_pdf).unwrap();

    fix(
        directory.path(),
        &[
            "--write",
            "--fix-exclude",
            "pdf",
            "--min-fix-confidence",
            "outdated",
        ],
    )
    .code(0)
    .stderr("");
    assert!(fs::read_to_string(text).unwrap().contains("/new?x=1&y=2"));
    assert_eq!(fs::read(&pdf_file).unwrap(), original_pdf);
}

#[tokio::test(flavor = "multi_thread")]
async fn encrypted_pdf_without_plaintext_url_is_refused_by_preflight() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("encrypted.pdf");
    let bytes = pdf("not-a-url", false, "/Encrypt 7 0 R", "");
    fs::write(&file, &bytes).unwrap();

    fix(&file, &["--write"])
        .code(1)
        .stderr(predicates::str::contains(
            "encrypted PDF files are not modified",
        ));
    assert_eq!(fs::read(file).unwrap(), bytes);
}

#[tokio::test(flavor = "multi_thread")]
async fn binary_backup_and_copy_preserve_original_and_write_fixed_copy() {
    let server = redirect_server().await;
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("fixture.docx");
    let original = ooxml("docx", &format!("{}{OLD_PATH}", server.uri()));
    fs::write(&file, &original).unwrap();

    fix(
        &file,
        &["--write", "--backup", "--min-fix-confidence", "outdated"],
    )
    .code(0)
    .stderr("");
    assert_eq!(fs::read(file.with_extension("docx.bak")).unwrap(), original);

    fs::write(&file, &original).unwrap();
    fix(&file, &["--copy", "--min-fix-confidence", "outdated"])
        .code(0)
        .stderr("");
    assert_eq!(fs::read(&file).unwrap(), original);
    assert_ne!(
        fs::read(directory.path().join("fixture.fixed.docx")).unwrap(),
        original
    );
}

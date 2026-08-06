use std::{env, fs, io::Write, path::Path};

use stalelink_core::{
    extract::{Extractor, HtmlExtractor, MarkdownExtractor, OoxmlExtractor, PdfExtractor, SourceDocument, TextExtractor},
    model::DocFormat,
};

fn main() {
    let mut args = env::args().skip(1);
    match (args.next().as_deref(), args.next()) {
        (Some("generate"), Some(directory)) => generate(Path::new(&directory)),
        (Some("extract"), Some(directory)) => extract_directory(Path::new(&directory)),
        _ => panic!("usage: stalelink-bench-harness <generate|extract> <directory>"),
    }
}

fn generate(directory: &Path) {
    let _ = fs::remove_dir_all(directory);
    fs::create_dir_all(directory).unwrap();
    let mut seed = 0x5EED_C0DE_u64;
    for (name, links) in [("small", 10), ("medium", 100), ("large", 500)] {
        write_text(directory, "md", name, links, &mut seed, |url| format!("[reference]({url})\n"));
        write_text(directory, "html", name, links, &mut seed, |url| format!("<a href=\"{url}\">reference</a>\n"));
        write_text(directory, "txt", name, links, &mut seed, |url| format!("Reference: {url}\n"));
        write_ooxml(directory, "docx", name, links, &mut seed);
        write_ooxml(directory, "xlsx", name, links, &mut seed);
        write_ooxml(directory, "pptx", name, links, &mut seed);
        write_pdf(directory, name, links, &mut seed);
    }
}

fn next_url(seed: &mut u64) -> String {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    format!("https://example.test/resources/{seed:016x}/guide")
}

fn write_text<F: Fn(&str) -> String>(dir: &Path, extension: &str, size: &str, links: usize, seed: &mut u64, format: F) {
    let mut text = String::from("# Generated benchmark document\nThis is deterministic prose around realistic references.\n");
    for _ in 0..links { text.push_str(&format(&next_url(seed))); }
    fs::write(dir.join(format!("{size}.{extension}")), text).unwrap();
}

fn write_ooxml(dir: &Path, extension: &str, size: &str, links: usize, seed: &mut u64) {
    let (part, relationships, mut body) = match extension {
        "docx" => (
            "word/document.xml",
            "word/_rels/document.xml.rels",
            String::from("<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><w:body>"),
        ),
        "xlsx" => (
            "xl/worksheets/sheet1.xml",
            "xl/worksheets/_rels/sheet1.xml.rels",
            String::from("<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><hyperlinks>"),
        ),
        _ => (
            "ppt/slides/slide1.xml",
            "ppt/slides/_rels/slide1.xml.rels",
            String::from("<p:sld xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">"),
        ),
    };
    let mut rels = String::from("<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">");
    for number in 1..=links {
        rels.push_str(&format!("<Relationship Id=\"rId{number}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink\" Target=\"{}\" TargetMode=\"External\"/>", next_url(seed)));
        match extension {
            "docx" => body.push_str(&format!("<w:p><w:hyperlink r:id=\"rId{number}\"><w:r><w:t>reference</w:t></w:r></w:hyperlink></w:p>")),
            "xlsx" => body.push_str(&format!("<hyperlink ref=\"A{number}\" r:id=\"rId{number}\"/>")),
            _ => body.push_str(&format!("<p:sp><p:nvSpPr><p:cNvPr id=\"{number}\" name=\"reference\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:txBody><a:p><a:r><a:rPr><a:hlinkClick r:id=\"rId{number}\"/></a:rPr><a:t>reference</a:t></a:r></a:p></p:txBody></p:sp>")),
        }
    }
    rels.push_str("</Relationships>");
    body.push_str(match extension { "docx" => "</w:body></w:document>", "xlsx" => "</hyperlinks></worksheet>", _ => "</p:sld>" });
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let mut entries = vec![(part, body), (relationships, rels)];
    if extension == "xlsx" {
        entries.push(("xl/workbook.xml", "<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets><sheet name=\"Generated\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>".into()));
        entries.push(("xl/_rels/workbook.xml.rels", "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Target=\"worksheets/sheet1.xml\"/></Relationships>".into()));
    }
    for (path, contents) in entries {
        zip.start_file(path, zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(contents.as_bytes()).unwrap();
    }
    fs::write(dir.join(format!("{size}.{extension}")), zip.finish().unwrap().into_inner()).unwrap();
}

fn write_pdf(dir: &Path, size: &str, links: usize, seed: &mut u64) {
    let mut stream = String::from("BT /F1 10 Tf 72 720 Td (Generated benchmark PDF) Tj ET\n");
    for _ in 0..links { stream.push_str(&format!("BT /F1 8 Tf 72 700 Td ({}) Tj ET\n", next_url(seed))); }
    let objects = ["<< /Type /Catalog /Pages 2 0 R >>".to_owned(), "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(), "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_owned(), format!("<< /Length {} >>\nstream\n{stream}endstream", stream.len()), "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned()];
    let mut pdf = b"%PDF-1.4\n".to_vec(); let mut offsets = vec![0];
    for (number, object) in objects.iter().enumerate() { offsets.push(pdf.len()); pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", number + 1).as_bytes()); }
    let xref = pdf.len(); pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets.iter().skip(1) { pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes()); }
    pdf.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    fs::write(dir.join(format!("{size}.pdf")), pdf).unwrap();
}

fn extract_directory(directory: &Path) {
    let mut total = 0;
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        let Some(format) = path.extension().and_then(|value| value.to_str()).and_then(format_of) else { continue };
        let bytes = fs::read(&path).unwrap();
        let document = SourceDocument {
            path,
            format,
            bytes,
        };
        let links = match format {
            DocFormat::Markdown => MarkdownExtractor.extract(&document), DocFormat::Html => HtmlExtractor.extract(&document),
            DocFormat::Text => TextExtractor.extract(&document), DocFormat::Pdf => PdfExtractor.extract(&document),
            DocFormat::Docx | DocFormat::Xlsx | DocFormat::Pptx => OoxmlExtractor.extract(&document),
        }.unwrap();
        total += links.len();
    }
    println!("{total}");
}

fn format_of(extension: &str) -> Option<DocFormat> { Some(match extension { "md" => DocFormat::Markdown, "html" => DocFormat::Html, "txt" => DocFormat::Text, "pdf" => DocFormat::Pdf, "docx" => DocFormat::Docx, "xlsx" => DocFormat::Xlsx, "pptx" => DocFormat::Pptx, _ => return None }) }

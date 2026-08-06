use std::{
    collections::BTreeMap,
    env, fs,
    hash::{DefaultHasher, Hash, Hasher},
    io::Write,
    path::Path,
    time::Instant,
};

use stalelink_core::{
    extract::{
        Extractor, HtmlExtractor, MarkdownExtractor, OoxmlExtractor, PdfExtractor, SourceDocument,
        TextExtractor,
    },
    model::DocFormat,
};

fn main() {
    let mut args = env::args().skip(1);
    match (args.next().as_deref(), args.next()) {
        (Some("generate"), Some(directory)) => generate(Path::new(&directory)),
        (Some("extract"), Some(directory)) => extract_directory(Path::new(&directory), 0, 1, None),
        (Some("throughput"), Some(directory)) => {
            let warmup = args.next().unwrap_or_else(|| "1".into()).parse().unwrap();
            let passes = args.next().unwrap_or_else(|| "5".into()).parse().unwrap();
            extract_directory(
                Path::new(&directory),
                warmup,
                passes,
                args.next().as_deref(),
            );
        }
        _ => panic!(
            "usage: stalelink-bench-harness <generate|extract|throughput> <directory> [warmup passes]"
        ),
    }
}

fn generate(directory: &Path) {
    let _ = fs::remove_dir_all(directory);
    fs::create_dir_all(directory).unwrap();
    let mut seed = 0x5EED_C0DE_u64;
    for (name, links) in [("small", 10), ("medium", 100), ("large", 500)] {
        for copy in 0..15 {
            let name = format!("{name}-{copy:02}");
            write_text(directory, "md", &name, links, &mut seed, |url| {
                match copy % 2 {
                    0 => format!("[reference]({url})\n"),
                    _ => format!("<{url}>\n"),
                }
            });
            write_text(directory, "html", &name, links, &mut seed, |url| {
                match copy % 3 {
                    0 => format!("<a href=\"{url}\">Tom &amp; Ada</a>\n"),
                    1 => format!("<link href='{url}' rel=\"alternate\">\n"),
                    _ => format!("<img src={url} alt=\"diagram\">\n"),
                }
            });
            write_text(directory, "txt", &name, links, &mut seed, |url| {
                format!("Reference in ordinary prose: {url}\n")
            });
            write_ooxml(directory, "docx", &name, links, &mut seed);
            write_ooxml(directory, "xlsx", &name, links, &mut seed);
            write_ooxml(directory, "pptx", &name, links, &mut seed);
            write_pdf(directory, &name, links, &mut seed, copy % 2 == 0);
        }
    }
}

fn next_url(seed: &mut u64) -> String {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    format!("https://example.test/resources/{seed:016x}/guide")
}

fn write_text<F: Fn(&str) -> String>(
    dir: &Path,
    extension: &str,
    size: &str,
    links: usize,
    seed: &mut u64,
    format: F,
) {
    let mut text = String::from(
        "# Generated benchmark document\nThis is deterministic prose around realistic references, entities like &amp;, and ordinary non-link content.\n",
    );
    for _ in 0..links {
        text.push_str(&format(&next_url(seed)));
    }
    fs::write(dir.join(format!("{size}.{extension}")), text).unwrap();
}

fn write_ooxml(dir: &Path, extension: &str, size: &str, links: usize, seed: &mut u64) {
    let (part, relationships, mut body) = match extension {
        "docx" => (
            "word/document.xml",
            "word/_rels/document.xml.rels",
            String::from(
                "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><w:body>",
            ),
        ),
        "xlsx" => (
            "xl/worksheets/sheet1.xml",
            "xl/worksheets/_rels/sheet1.xml.rels",
            String::from(
                "<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><hyperlinks>",
            ),
        ),
        _ => (
            "ppt/slides/slide1.xml",
            "ppt/slides/_rels/slide1.xml.rels",
            String::from(
                "<p:sld xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">",
            ),
        ),
    };
    let mut rels = String::from(
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    );
    rels.push_str("<Relationship Id=\"rIdIgnored\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/image1.png\"/>");
    for number in 1..=links {
        rels.push_str(&format!("<Relationship Id=\"rId{number}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink\" Target=\"{}\" TargetMode=\"External\"/>", next_url(seed)));
        match extension {
            "docx" => body.push_str(&format!("<w:p><w:hyperlink r:id=\"rId{number}\"><w:r><w:t>reference</w:t></w:r></w:hyperlink></w:p>")),
            "xlsx" => body.push_str(&format!("<hyperlink ref=\"A{number}\" r:id=\"rId{number}\"/>")),
            _ => body.push_str(&format!("<p:sp><p:nvSpPr><p:cNvPr id=\"{number}\" name=\"reference\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:txBody><a:p><a:r><a:rPr><a:hlinkClick r:id=\"rId{number}\"/></a:rPr><a:t>reference</a:t></a:r></a:p></p:txBody></p:sp>")),
        }
    }
    rels.push_str("</Relationships>");
    body.push_str(match extension {
        "docx" => "</w:body></w:document>",
        "xlsx" => "</hyperlinks></worksheet>",
        _ => "</p:sld>",
    });
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let mut entries = vec![(part, body), (relationships, rels)];
    if extension == "xlsx" {
        entries.push(("xl/workbook.xml", "<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets><sheet name=\"Generated\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>".into()));
        entries.push(("xl/_rels/workbook.xml.rels", "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Target=\"worksheets/sheet1.xml\"/></Relationships>".into()));
    }
    for (path, contents) in entries {
        zip.start_file(path, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(contents.as_bytes()).unwrap();
    }
    fs::write(
        dir.join(format!("{size}.{extension}")),
        zip.finish().unwrap().into_inner(),
    )
    .unwrap();
}

fn write_pdf(dir: &Path, size: &str, links: usize, seed: &mut u64, compressed: bool) {
    let mut stream = String::from("BT /F1 10 Tf 72 720 Td (Generated benchmark PDF) Tj ET\n");
    for _ in 0..links {
        stream.push_str(&format!(
            "BT /F1 8 Tf 72 700 Td ({}) Tj ET\n",
            next_url(seed)
        ));
    }
    let stream = if compressed {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(stream.as_bytes()).unwrap();
        encoder.finish().unwrap()
    } else {
        stream.into_bytes()
    };
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 6 0 R >> >> /Contents 4 0 R /Annots [5 0 R] >>"
            .to_owned(),
        format!(
            "<< /Length {} {} >>\nstream\n",
            stream.len(),
            if compressed {
                "/Filter /FlateDecode"
            } else {
                ""
            }
        ),
        "<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] /A << /S /URI /URI (https://annotation.example.test/retained) >> >>".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0];
    for (number, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{object}", number + 1).as_bytes());
        if number == 3 {
            pdf.extend_from_slice(&stream);
            pdf.extend_from_slice(b"\nendstream");
        }
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets.iter().skip(1) {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    fs::write(dir.join(format!("{size}.pdf")), pdf).unwrap();
}

fn all_links(
    directory: &Path,
    format_filter: Option<&str>,
) -> Vec<stalelink_core::model::FoundLink> {
    let mut links = Vec::new();
    let mut paths = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
            continue;
        };
        if format_filter.is_some_and(|filter| filter != extension) {
            continue;
        }
        let Some(format) = format_of(extension) else {
            continue;
        };
        let bytes = fs::read(&path).unwrap();
        let document = SourceDocument {
            path,
            format,
            bytes,
        };
        let extracted = match format {
            DocFormat::Markdown => MarkdownExtractor.extract(&document),
            DocFormat::Html => HtmlExtractor.extract(&document),
            DocFormat::Text => TextExtractor.extract(&document),
            DocFormat::Pdf => PdfExtractor.extract(&document),
            DocFormat::Docx | DocFormat::Xlsx | DocFormat::Pptx => {
                OoxmlExtractor.extract(&document)
            }
        }
        .unwrap();
        links.extend(extracted);
    }
    links.sort_by_key(|link| serde_json::to_string(link).unwrap());
    links
}

fn extract_directory(directory: &Path, warmup: usize, passes: usize, format_filter: Option<&str>) {
    for _ in 0..warmup {
        let _ = all_links(directory, format_filter);
    }
    let mut elapsed = Vec::new();
    let mut links = Vec::new();
    for _ in 0..passes {
        let start = Instant::now();
        links = all_links(directory, format_filter);
        elapsed.push(start.elapsed().as_secs_f64());
    }
    elapsed.sort_by(f64::total_cmp);
    let mut hasher = DefaultHasher::new();
    let records = links
        .iter()
        .map(|link| {
            serde_json::json!({
                "doc": link.source.path.file_name().unwrap().to_string_lossy(),
                "url": link.url,
                "location": link.source.location,
                "span": link.source.byte_span.as_ref().map(|span| [span.start, span.end]),
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&records).unwrap().hash(&mut hasher);
    let mut formats = BTreeMap::<String, usize>::new();
    for link in &links {
        *formats
            .entry(format!("{:?}", link.source.format).to_ascii_lowercase())
            .or_default() += 1;
    }
    println!(
        "{}",
        serde_json::json!({
            "documents": if format_filter.is_some() { links.iter().map(|link| &link.source.path).collect::<std::collections::HashSet<_>>().len() } else { fs::read_dir(directory).unwrap().count() },
            "links": links.len(),
            "digest": format!("{:016x}", hasher.finish()),
            "records": records,
            "median_seconds": elapsed[elapsed.len() / 2],
            "formats": formats,
        })
    );
}

fn format_of(extension: &str) -> Option<DocFormat> {
    Some(match extension {
        "md" => DocFormat::Markdown,
        "html" => DocFormat::Html,
        "txt" => DocFormat::Text,
        "pdf" => DocFormat::Pdf,
        "docx" => DocFormat::Docx,
        "xlsx" => DocFormat::Xlsx,
        "pptx" => DocFormat::Pptx,
        _ => return None,
    })
}

//! Any pile of files -> documents holding plain text or speaker-labelled messages.
//!
//! The format is decided from the bytes first and the file name second, so misnamed files still work.
//! A file that cannot be read is reported in [`Corpus::unreadable`] and never stops the others.

mod dataset;
mod markup;
mod office;
mod text;

pub(crate) use dataset::{SYNTH_BASE_MS, SYNTH_RECORD_GAP_MS};

use crate::{Conversation, Error, Result};
use dataset::ParsedFile;
use serde_json::{Map, Value};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

const MAX_NESTING: usize = 3;
const WORKER_STACK_BYTES: usize = 32 << 20; // PDF parsers recurse deeply on nested objects
const MEDIA_SIGNATURES: &[&[u8]] = &[b"\x89PNG", b"\xFF\xD8\xFF", b"GIF8", b"ID3\x03", b"ID3\x04", b"OggS", b"RIFF"];

#[derive(Debug, Clone, Default)]
pub struct ParseOptions {
    /// Extra substrings marking system lines to drop, for exports not in English. The built-in
    /// English markers always apply.
    pub system_markers: Vec<String>,
}

pub struct Document {
    pub name: String,
    pub format: &'static str,
    pub content: Content,
}

pub enum Content {
    /// The source says who wrote each message.
    Speakers(Conversation),
    Text(String),
}

pub struct Unreadable {
    pub name: String,
    pub reason: String,
}

pub struct Corpus {
    pub documents: Vec<Document>,
    pub unreadable: Vec<Unreadable>,
}

/// What reading one file produced. Structured data is merged across files at the end.
#[derive(Default)]
struct Sink {
    texts: Vec<Document>,
    data: Vec<ParsedFile>,
    unreadable: Vec<Unreadable>,
}

impl Sink {
    fn text(&mut self, name: &str, format: &'static str, text: String) {
        if text.trim().is_empty() {
            self.fail(name, "no text found in this file".into());
        } else {
            self.texts.push(Document { name: name.to_string(), format, content: Content::Text(text) });
        }
    }

    fn fail(&mut self, name: &str, reason: String) {
        self.unreadable.push(Unreadable { name: name.to_string(), reason });
    }
}

/// Read files and folders, recursively.
pub fn ingest<P: AsRef<Path>>(paths: &[P], opts: &ParseOptions) -> Result<Corpus> {
    let mut found = Vec::new();
    for p in paths {
        collect(p.as_ref(), &mut found)?;
    }
    let mut files = Vec::with_capacity(found.len());
    for f in found {
        let bytes = std::fs::read(&f).map_err(|e| Error::Io(format!("{}: {e}", f.display())))?;
        let name = f.file_name().map_or_else(|| f.display().to_string(), |n| n.to_string_lossy().into_owned());
        files.push((name, bytes));
    }
    ingest_files(&files, opts)
}

fn collect(p: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !p.is_dir() {
        out.push(p.to_path_buf());
        return Ok(());
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(p)
        .map_err(|e| Error::Io(format!("{}: {e}", p.display())))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for e in entries {
        collect(&e, out)?;
    }
    Ok(())
}

/// Read named in-memory files. Fails only when nothing at all could be read.
pub fn ingest_files(files: &[(String, Vec<u8>)], opts: &ParseOptions) -> Result<Corpus> {
    let sinks = in_parallel(files, |(name, bytes)| {
        let mut sink = Sink::default();
        // a parser panic on malformed input must cost one file, not the whole import
        if catch_unwind(AssertUnwindSafe(|| read_any(name, bytes, opts, 0, &mut sink))).is_err() {
            sink.fail(name, "this file is damaged or uses an unsupported variant of its format".into());
        }
        sink
    });

    let (mut documents, mut data, mut unreadable) = (Vec::new(), Vec::new(), Vec::new());
    for sink in sinks {
        documents.extend(sink.texts);
        data.extend(sink.data);
        unreadable.extend(sink.unreadable);
    }
    for m in dataset::merge(data) {
        let content = if m.has_senders {
            Content::Speakers(m.conversation)
        } else {
            let texts: Vec<String> = m.conversation.messages.into_iter().map(|t| t.text).collect();
            Content::Text(texts.join("\n\n"))
        };
        documents.push(Document { name: m.name, format: m.format, content });
    }
    if documents.is_empty() {
        let why: Vec<String> = unreadable.iter().map(|u| format!("{}: {}", u.name, u.reason)).collect();
        return Err(Error::Invalid(if why.is_empty() { "no files given".into() } else { why.join("; ") }));
    }
    Ok(Corpus { documents, unreadable })
}

fn in_parallel<T: Sync, R: Send>(items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<R>>> = Mutex::new(items.iter().map(|_| None).collect());
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get()).min(items.len().max(1));
    std::thread::scope(|s| {
        for _ in 0..workers {
            std::thread::Builder::new()
                .stack_size(WORKER_STACK_BYTES)
                .spawn_scoped(s, || loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    let r = work(item);
                    results.lock().unwrap_or_else(|e| e.into_inner())[i] = Some(r);
                })
                .expect("spawning a reader thread");
        }
    });
    results.into_inner().unwrap_or_else(|e| e.into_inner()).into_iter().flatten().collect()
}

/// Decide the format and read one file.
fn read_any(name: &str, bytes: &[u8], opts: &ParseOptions, depth: usize, out: &mut Sink) {
    let head = &bytes[..bytes.len().min(1024)];
    let result = if bytes.is_empty() {
        Err("the file is empty".to_string())
    } else if head.windows(5).any(|w| w == b"%PDF-") {
        read_pdf(name, bytes, out)
    } else if bytes.starts_with(b"PK\x03\x04") {
        office::read_zip(name, bytes, opts, depth, out)
    } else if bytes.starts_with(b"\xD0\xCF\x11\xE0") {
        office::read_ole(bytes).map(|(format, text)| out.text(name, format, text))
    } else if bytes.starts_with(b"{\\rtf") {
        out.text(name, "RTF", text::rtf(bytes));
        Ok(())
    } else if MEDIA_SIGNATURES.iter().any(|m| bytes.starts_with(m)) || bytes.get(4..8) == Some(b"ftyp") {
        Err("images, audio and video are not supported".to_string())
    } else if bytes.get(60..68) == Some(b"BOOKMOBI") {
        Err("Kindle files are not supported; convert it to EPUB".to_string())
    } else {
        read_textual(name, bytes, opts, out)
    };
    if let Err(reason) = result {
        out.fail(name, reason);
    }
}

fn read_pdf(name: &str, bytes: &[u8], out: &mut Sink) -> std::result::Result<(), String> {
    let raw = pdf_extract::extract_text_from_mem(bytes).map_err(|e| format!("could not read this PDF: {e}"))?;
    let clean = text::strip_page_furniture(&raw);
    if clean.chars().filter(|c| c.is_alphanumeric()).count() < 16 {
        return Err("this PDF has no text layer (it is probably scanned pages)".into());
    }
    out.text(name, "PDF", clean);
    Ok(())
}

fn read_textual(name: &str, bytes: &[u8], opts: &ParseOptions, out: &mut Sink) -> std::result::Result<(), String> {
    let decoded = text::decode(bytes);
    if text::looks_binary(&decoded) {
        return Err("unsupported binary format".into());
    }
    let extension = name.rsplit('.').next().unwrap_or("").to_lowercase();
    let start = decoded.trim_start().get(..64).unwrap_or(decoded.trim_start()).to_lowercase();

    let json = match extension.as_str() {
        "json" | "jsonl" | "ndjson" => Some(dataset::parse_json(decoded.as_bytes(), opts).map_err(|e| e.to_string())?),
        // misnamed JSON; text that merely starts like it stays text
        _ if start.starts_with(['{', '[']) => dataset::parse_json(decoded.as_bytes(), opts).ok(),
        _ => None,
    };
    if let Some(parsed) = json {
        out.data.push((name.to_string(), "JSON", parsed));
        return Ok(());
    }

    match extension.as_str() {
        "csv" => read_table(name, "CSV", text::parse_delimited(&decoded, ','), opts, out),
        "tsv" => read_table(name, "TSV", text::parse_delimited(&decoded, '\t'), opts, out),
        "srt" | "vtt" => out.text(name, "Subtitles", text::subtitles(&decoded)),
        "html" | "htm" | "xhtml" => out.text(name, "HTML", markup::extract(&decoded, &markup::HTML)),
        _ if start.starts_with("<!doctype html") || start.starts_with("<html") => {
            out.text(name, "HTML", markup::extract(&decoded, &markup::HTML))
        }
        _ if start.starts_with("<?xml") || extension == "xml" || extension == "fb2" => {
            out.text(name, "XML", markup::extract(&decoded, &markup::GENERIC_XML))
        }
        _ => out.text(name, "Text", decoded),
    }
    Ok(())
}

/// A table with recognisable columns (speaker, text, ...) is a dataset; otherwise each row is a line.
fn read_table(name: &str, format: &'static str, rows: Vec<Vec<String>>, opts: &ParseOptions, out: &mut Sink) {
    let Some((header, body)) = rows.split_first() else {
        return out.fail(name, "the table is empty".into());
    };
    let records: Vec<Value> = body
        .iter()
        .map(|row| {
            let cells = header.iter().zip(row).map(|(h, c)| (h.trim().to_string(), Value::String(c.clone())));
            Value::Object(cells.collect::<Map<String, Value>>())
        })
        .collect();
    let parsed = dataset::parse_rows(&records, opts);
    if parsed.messages.is_empty() {
        let lines: Vec<String> = rows.iter().map(|r| r.join(" | ")).collect();
        out.text(name, format, lines.join("\n\n"));
    } else {
        out.data.push((name.to_string(), format, parsed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn read(name: &str, bytes: &[u8]) -> Corpus {
        ingest_files(&[(name.to_string(), bytes.to_vec())], &ParseOptions::default()).unwrap()
    }

    fn text_of(c: &Corpus) -> &str {
        match &c.documents[0].content {
            Content::Text(t) => t,
            Content::Speakers(_) => panic!("expected text"),
        }
    }

    fn zip_of(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, body) in entries {
            z.start_file(*name, SimpleFileOptions::default()).unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap().into_inner()
    }

    #[test]
    fn format_comes_from_content_not_name() {
        let docx = zip_of(&[("word/document.xml", "<w:p><w:r><w:t>Hello</w:t></w:r></w:p>")]);
        let c = read("renamed.txt", &docx);
        assert_eq!((c.documents[0].format, text_of(&c)), ("Word", "Hello"));
        assert_eq!(read("page", b"<!DOCTYPE html><body><p>Hi</p></body>").documents[0].format, "HTML");
        assert_eq!(read("notes.dat", b"just some notes").documents[0].format, "Text");
        assert_eq!(read("cars.txt", b"BMW builds cars in Bavaria.").documents[0].format, "Text");
        assert_eq!(read("data.txt", br#"["one text"]"#).documents[0].format, "JSON");
        assert_eq!(read("log.txt", b"[info] started").documents[0].format, "Text");
    }

    #[test]
    fn epub_follows_spine_order() {
        let epub = zip_of(&[
            ("META-INF/container.xml", r#"<container><rootfiles><rootfile full-path="OPS/book.opf"/></rootfiles></container>"#),
            ("OPS/book.opf", r#"<package><manifest><item id="1" href="z.xhtml"/><item id="2" href="a.xhtml"/></manifest><spine><itemref idref="1"/><itemref idref="2"/></spine></package>"#),
            ("OPS/a.xhtml", "<html><body><p>second</p></body></html>"),
            ("OPS/z.xhtml", "<html><body><p>first</p></body></html>"),
        ]);
        assert_eq!(text_of(&read("b.epub", &epub)), "first\n\nsecond");
    }

    #[test]
    fn slides_sort_numerically() {
        let slide = |t: &str| format!("<a:p><a:r><a:t>{t}</a:t></a:r></a:p>");
        let pptx = zip_of(&[("ppt/slides/slide10.xml", &slide("ten")), ("ppt/slides/slide2.xml", &slide("two"))]);
        assert_eq!(text_of(&read("d.pptx", &pptx)), "two\n\nten");
    }

    #[test]
    fn xlsx_with_speaker_column_is_structured() {
        let xlsx = zip_of(&[
            ("xl/workbook.xml", "<workbook/>"),
            ("xl/sharedStrings.xml", "<sst><si><t>speaker</t></si><si><t>text</t></si><si><r><t>Ma</t></r><r><t>rta</t></r></si><si><t>Late.</t></si></sst>"),
            ("xl/worksheets/sheet1.xml", r#"<worksheet><sheetData><row><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row><row><c r="A2" t="s"><v>2</v></c><c r="B2" t="s"><v>3</v></c></row></sheetData></worksheet>"#),
        ]);
        let c = read("t.xlsx", &xlsx);
        let Content::Speakers(conv) = &c.documents[0].content else { panic!("expected speakers") };
        assert_eq!((conv.messages[0].sender.as_str(), conv.messages[0].text.as_str()), ("Marta", "Late."));
    }

    #[test]
    fn csv_without_known_columns_is_text() {
        assert_eq!(text_of(&read("t.csv", b"city,pop\nSeoul,9\n")), "city | pop\n\nSeoul | 9");
        let c = read("t.csv", b"character,dialogue\nMarta,Hi\nTobias,Hello\n");
        assert!(matches!(c.documents[0].content, Content::Speakers(_)));
    }

    #[test]
    fn archive_reads_entries_and_skips_media() {
        let inner = zip_of(&[("deep.txt", "deep text")]);
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, body) in [("a.txt", &b"alpha"[..]), ("photo.jpg", b"\xFF\xD8\xFFxx"), ("nested.zip", &inner)] {
            z.start_file(name, SimpleFileOptions::default()).unwrap();
            z.write_all(body).unwrap();
        }
        let c = read("all.zip", &z.finish().unwrap().into_inner());
        assert_eq!(c.documents.len(), 2);
        assert!(c.unreadable.is_empty());
    }

    #[test]
    fn bad_files_are_reported_not_fatal() {
        let files = vec![("good.txt".to_string(), b"fine".to_vec()), ("pic.png".to_string(), b"\x89PNGxx".to_vec()), ("bad.pdf".to_string(), b"%PDF-1.7 garbage".to_vec())];
        let c = ingest_files(&files, &ParseOptions::default()).unwrap();
        assert_eq!(c.documents.len(), 1);
        assert_eq!(c.unreadable.len(), 2);
        assert!(ingest_files(&[("x.png".to_string(), b"\x89PNG".to_vec())], &ParseOptions::default()).is_err());
    }
}

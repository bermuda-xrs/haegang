//! Containers: zip (DOCX, PPTX, XLSX, ODF, EPUB, HWPX, archives) and OLE (.doc, .hwp).

use super::markup::{self, attr, events, is_self_closed, local_name, Event, Profile};
use super::{read_any, text, Sink};
use super::ParseOptions;
use cfb::CompoundFile;
use flate2::read::DeflateDecoder;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use zip::ZipArchive;

const MAX_ENTRY_BYTES: u64 = 512 << 20;
/// Skipped silently inside archives, which are full of them.
const MEDIA: &[&str] =
    &["jpg", "jpeg", "png", "gif", "webp", "heic", "bmp", "svg", "mp4", "mov", "webm", "mp3", "m4a", "wav", "ogg", "opus"];

type Zip<'a> = ZipArchive<Cursor<&'a [u8]>>;

const WORD_LIKE: Profile =
    Profile { text: &["t"], blocks: &["p"], breaks: &["br", "cr"], spaces: &["tab"], skip: &["instrtext", "rph"] };
const ODF: Profile = Profile {
    text: &["p", "h"],
    blocks: &["p", "h"],
    breaks: &["line-break"],
    spaces: &["s", "tab"],
    skip: &["tracked-changes", "annotation"],
};

fn entry(z: &mut Zip, name: &str) -> Result<String, String> {
    let file = z.by_name(name).map_err(|e| format!("{name}: {e}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_ENTRY_BYTES).read_to_end(&mut bytes).map_err(|e| format!("{name}: {e}"))?;
    Ok(text::decode(&bytes))
}

fn names_with(z: &Zip, prefix: &str, suffix: &str) -> Vec<String> {
    let mut names: Vec<String> = z
        .file_names()
        .filter_map(|n| n.ok())
        .filter(|n| n.starts_with(prefix) && n.ends_with(suffix))
        .map(|n| n.into_owned())
        .collect();
    // slide2 before slide10
    names.sort_by_key(|n| (n.len(), n.clone()));
    names
}

pub(super) fn read_zip(name: &str, bytes: &[u8], opts: &ParseOptions, depth: usize, out: &mut Sink) -> Result<(), String> {
    let mut z = ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("not a readable zip file: {e}"))?;
    let has = |z: &Zip, n: &str| z.index_for_name(n).is_some();
    if has(&z, "word/document.xml") {
        let xml = entry(&mut z, "word/document.xml")?;
        out.text(name, "Word", markup::extract(&xml, &WORD_LIKE));
    } else if has(&z, "ppt/presentation.xml") || !names_with(&z, "ppt/slides/slide", ".xml").is_empty() {
        let mut slides = Vec::new();
        for slide in names_with(&z, "ppt/slides/slide", ".xml") {
            slides.push(markup::extract(&entry(&mut z, &slide)?, &WORD_LIKE));
        }
        out.text(name, "PowerPoint", slides.join("\n\n"));
    } else if has(&z, "xl/workbook.xml") {
        read_xlsx(name, &mut z, opts, out)?;
    } else if has(&z, "META-INF/container.xml") {
        out.text(name, "EPUB", read_epub(&mut z)?);
    } else if has(&z, "content.xml") && has(&z, "mimetype") {
        let xml = entry(&mut z, "content.xml")?;
        out.text(name, "OpenDocument", markup::extract(&xml, &ODF));
    } else if !names_with(&z, "Contents/section", ".xml").is_empty() {
        let mut sections = Vec::new();
        for s in names_with(&z, "Contents/section", ".xml") {
            sections.push(markup::extract(&entry(&mut z, &s)?, &WORD_LIKE));
        }
        out.text(name, "Hangul", sections.join("\n\n"));
    } else if depth >= super::MAX_NESTING {
        return Err("archive nested too deeply".into());
    } else {
        read_archive(name, &mut z, opts, depth, out)?;
    }
    Ok(())
}

fn read_archive(name: &str, z: &mut Zip, opts: &ParseOptions, depth: usize, out: &mut Sink) -> Result<(), String> {
    let mut any = false;
    for i in 0..z.len() {
        let mut file = z.by_index(i).map_err(|e| e.to_string())?;
        let entry_name = file.name().map_err(|e| e.to_string())?.into_owned();
        let extension = entry_name.rsplit('.').next().unwrap_or("").to_lowercase();
        if file.is_dir() || MEDIA.contains(&extension.as_str()) {
            continue;
        }
        let mut bytes = Vec::new();
        file.by_ref().take(MAX_ENTRY_BYTES).read_to_end(&mut bytes).map_err(|e| format!("{entry_name}: {e}"))?;
        drop(file);
        read_any(&format!("{name}/{entry_name}"), &bytes, opts, depth + 1, out);
        any = true;
    }
    if any { Ok(()) } else { Err("no readable files in this archive".into()) }
}

fn resolve(base_file: &str, href: &str) -> String {
    let href = href.split('#').next().unwrap_or(href).replace("%20", " ");
    let mut parts: Vec<&str> = base_file.split('/').collect();
    parts.pop();
    for seg in href.split('/') {
        match seg {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn read_epub(z: &mut Zip) -> Result<String, String> {
    let container = entry(z, "META-INF/container.xml")?;
    let opf_path = events(&container)
        .find_map(|e| match e {
            Event::Open(t) if local_name(t) == "rootfile" => attr(t, "full-path"),
            _ => None,
        })
        .ok_or("EPUB without a package file")?;
    let opf = entry(z, &opf_path)?;
    let mut manifest: HashMap<String, String> = HashMap::new();
    let mut spine = Vec::new();
    for e in events(&opf) {
        if let Event::Open(t) = e {
            match local_name(t) {
                "item" => {
                    if let (Some(id), Some(href)) = (attr(t, "id"), attr(t, "href")) {
                        manifest.insert(id, href);
                    }
                }
                "itemref" => spine.extend(attr(t, "idref")),
                _ => {}
            }
        }
    }
    let mut chapters = Vec::new();
    for id in spine {
        if let Some(href) = manifest.get(&id) {
            let path = resolve(&opf_path, href);
            if let Ok(xhtml) = entry(z, &path) {
                chapters.push(markup::extract(&xhtml, &markup::HTML));
            }
        }
    }
    Ok(chapters.join("\n\n"))
}

fn column(cell: &str) -> usize {
    cell.chars().take_while(char::is_ascii_alphabetic).fold(0, |n, c| n * 26 + (c.to_ascii_uppercase() as usize - 64)).saturating_sub(1)
}

fn read_xlsx(name: &str, z: &mut Zip, opts: &ParseOptions, out: &mut Sink) -> Result<(), String> {
    let mut shared = Vec::new();
    if z.index_for_name("xl/sharedStrings.xml").is_some() {
        let xml = entry(z, "xl/sharedStrings.xml")?;
        let (mut item, mut inside, mut phonetic) = (String::new(), false, false);
        for e in events(&xml) {
            match e {
                Event::Open(t) if local_name(t) == "rPh" => phonetic = true,
                Event::Close("rPh") => phonetic = false,
                Event::Open(t) if local_name(t) == "t" => inside = !phonetic && !is_self_closed(t),
                Event::Close("t") => inside = false,
                Event::Text(t) | Event::Raw(t) if inside => item.push_str(&markup::decode_entities(t)),
                Event::Close("si") => shared.push(std::mem::take(&mut item)),
                _ => {}
            }
        }
    }
    for sheet in names_with(z, "xl/worksheets/sheet", ".xml") {
        let xml = entry(z, &sheet)?;
        let (mut rows, mut row): (Vec<Vec<String>>, Vec<String>) = (Vec::new(), Vec::new());
        let (mut col, mut kind, mut value, mut inside) = (0usize, String::new(), String::new(), false);
        for e in events(&xml) {
            match e {
                Event::Open(t) => match local_name(t) {
                    "c" if !is_self_closed(t) => {
                        col = attr(t, "r").map_or(row.len(), |r| column(&r));
                        kind = attr(t, "t").unwrap_or_default();
                        value.clear();
                    }
                    "v" | "t" => inside = true,
                    _ => {}
                },
                Event::Text(t) | Event::Raw(t) if inside => value.push_str(&markup::decode_entities(t)),
                Event::Close("v" | "t") => inside = false,
                Event::Close("c") => {
                    let cell = match kind.as_str() {
                        "s" => value.trim().parse::<usize>().ok().and_then(|i| shared.get(i).cloned()).unwrap_or_default(),
                        _ => std::mem::take(&mut value),
                    };
                    if row.len() <= col {
                        row.resize(col + 1, String::new());
                    }
                    row[col] = cell;
                }
                Event::Close("row") => rows.push(std::mem::take(&mut row)),
                _ => {}
            }
        }
        rows.retain(|r| r.iter().any(|c| !c.trim().is_empty()));
        if !rows.is_empty() {
            super::read_table(name, "Excel", rows, opts, out);
        }
    }
    Ok(())
}

type Ole<'a> = CompoundFile<Cursor<&'a [u8]>>;

fn stream(ole: &mut Ole, path: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    ole.open_stream(path).map_err(|e| format!("{path}: {e}"))?.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

pub(super) fn read_ole(bytes: &[u8]) -> Result<(&'static str, String), String> {
    let mut ole = CompoundFile::open(Cursor::new(bytes)).map_err(|e| format!("not a readable document: {e}"))?;
    if ole.is_stream("/WordDocument") {
        Ok(("Word 97-2003", read_doc(&mut ole)?))
    } else if ole.is_stream("/FileHeader") && ole.is_storage("/BodyText") {
        Ok(("Hangul", read_hwp(&mut ole)?))
    } else if ole.is_stream("/PowerPoint Document") {
        Err("old .ppt files are not supported; save it as .pptx".into())
    } else if ole.is_stream("/Workbook") || ole.is_stream("/Book") {
        Err("old .xls files are not supported; save it as .xlsx".into())
    } else {
        Err("unknown legacy document type".into())
    }
}

fn u16_at(b: &[u8], i: usize) -> Option<usize> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?) as usize)
}

fn u32_at(b: &[u8], i: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?) as usize)
}

/// Word 97-2003: the piece table maps text positions into the WordDocument stream ([MS-DOC]).
fn read_doc(ole: &mut Ole) -> Result<String, String> {
    const BAD: &str = "damaged Word file";
    let word = stream(ole, "/WordDocument")?;
    let flags = u16_at(&word, 0x0A).ok_or(BAD)?;
    if flags & 0x0100 != 0 {
        return Err("this Word file is password protected".into());
    }
    let table = stream(ole, if flags & 0x0200 != 0 { "/1Table" } else { "/0Table" })?;
    let main_chars = u32_at(&word, 0x4C).ok_or(BAD)?;
    let (fc_clx, lcb_clx) = (u32_at(&word, 0x01A2).ok_or(BAD)?, u32_at(&word, 0x01A6).ok_or(BAD)?);
    let mut clx = table.get(fc_clx..fc_clx + lcb_clx).ok_or(BAD)?;
    while clx.first() == Some(&1) {
        clx = clx.get(3 + u16_at(clx, 1).ok_or(BAD)?..).ok_or(BAD)?; // formatting runs before the piece table
    }
    if clx.first() != Some(&2) {
        return Err(BAD.into());
    }
    let lcb = u32_at(clx, 1).ok_or(BAD)?;
    let plc = clx.get(5..5 + lcb).ok_or(BAD)?;
    let pieces = lcb.saturating_sub(4) / 12;
    let mut out = String::new();
    for i in 0..pieces {
        let (start, end) = (u32_at(plc, i * 4).ok_or(BAD)?, u32_at(plc, (i + 1) * 4).ok_or(BAD)?);
        if start >= main_chars {
            break;
        }
        let count = end.min(main_chars).saturating_sub(start);
        let fc = u32_at(plc, (pieces + 1) * 4 + i * 8 + 2).ok_or(BAD)?;
        let offset = fc & 0x3FFF_FFFF;
        if fc & 0x4000_0000 != 0 {
            let bytes = word.get(offset / 2..offset / 2 + count).ok_or(BAD)?;
            out.push_str(&encoding_rs::WINDOWS_1252.decode_without_bom_handling(bytes).0);
        } else {
            let bytes = word.get(offset..offset + count * 2).ok_or(BAD)?;
            out.push_str(&encoding_rs::UTF_16LE.decode_without_bom_handling(bytes).0);
        }
    }
    Ok(clean_doc_text(&out))
}

/// Paragraph and cell marks become breaks; field codes (`\x13 code \x14 result \x15`) keep only the result.
fn clean_doc_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_field_code = 0usize;
    for c in s.chars() {
        match c {
            '\u{13}' => in_field_code += 1,
            '\u{14}' | '\u{15}' => in_field_code = in_field_code.saturating_sub(1),
            _ if in_field_code > 0 => {}
            '\r' | '\u{7}' | '\u{c}' => out.push_str("\n\n"),
            '\u{b}' => out.push('\n'),
            c if c.is_control() && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

/// Hangul 5.x: sections are (usually deflated) record streams; PARA_TEXT records hold the UTF-16 text.
fn read_hwp(ole: &mut Ole) -> Result<String, String> {
    const PARA_TEXT: usize = 67;
    let header = stream(ole, "/FileHeader")?;
    let props = u32_at(&header, 36).ok_or("damaged Hangul file")?;
    if props & 0b10 != 0 {
        return Err("this Hangul file is password protected".into());
    }
    let mut out = String::new();
    for section in 0.. {
        let path = format!("/BodyText/Section{section}");
        if !ole.is_stream(&path) {
            break;
        }
        let raw = stream(ole, &path)?;
        let data = if props & 1 != 0 {
            let mut inflated = Vec::new();
            DeflateDecoder::new(&raw[..]).read_to_end(&mut inflated).map_err(|e| e.to_string())?;
            inflated
        } else {
            raw
        };
        let mut i = 0;
        while let Some(header) = u32_at(&data, i) {
            i += 4;
            let tag = header & 0x3FF;
            let mut size = header >> 20;
            if size == 0xFFF {
                size = u32_at(&data, i).ok_or("damaged Hangul file")?;
                i += 4;
            }
            let body = data.get(i..i + size).ok_or("damaged Hangul file")?;
            i += size;
            if tag == PARA_TEXT {
                push_hwp_paragraph(body, &mut out);
            }
        }
    }
    Ok(out)
}

fn push_hwp_paragraph(body: &[u8], out: &mut String) {
    let units: Vec<u16> = body.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    let mut text: Vec<u16> = Vec::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        match units[i] {
            10 => text.push(10),
            0 | 13 | 24..=31 => {}
            // inline and extended controls occupy eight code units
            1..=9 | 11 | 12 | 14..=23 => i += 7,
            u => text.push(u),
        }
        i += 1;
    }
    out.push_str(&String::from_utf16_lossy(&text));
    out.push_str("\n\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn hwp_paragraph_skips_controls() {
        let mut units: Vec<u16> = "안".encode_utf16().collect();
        units.extend([2u16, 0x7365, 0x6374, 0, 0, 0, 0, 2]); // an extended control, 8 units
        units.extend("녕".encode_utf16());
        units.push(13);
        let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
        let mut out = String::new();
        push_hwp_paragraph(&bytes, &mut out);
        assert_eq!(out, "안녕\n\n");
    }

    #[test]
    fn doc_field_codes_and_marks() {
        assert_eq!(clean_doc_text("One\r\u{13} HYPERLINK x \u{14}link\u{15} two\u{7}"), "One\n\nlink two\n\n");
    }

    #[test]
    fn hrefs_and_cells() {
        assert_eq!(resolve("OEBPS/content.opf", "text/ch%201.xhtml#a"), "OEBPS/text/ch 1.xhtml");
        assert_eq!(resolve("a/b/c.opf", "../d.xhtml"), "a/d.xhtml");
        assert_eq!((column("A1"), column("B7"), column("AA3")), (0, 1, 26));
    }

    /// A minimal real .doc container: one UTF-16 piece, one 8-bit piece.
    #[test]
    fn doc_piece_table() {
        let text1: Vec<u8> = "한글 ".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let text2 = b"plain\r";
        let mut word = vec![0u8; 0x400];
        word[0x0A..0x0C].copy_from_slice(&0x0200u16.to_le_bytes()); // table stream is 1Table
        let (o1, o2) = (word.len(), word.len() + text1.len());
        word.extend_from_slice(&text1);
        word.extend_from_slice(text2);
        word[0x4C..0x50].copy_from_slice(&9u32.to_le_bytes());

        let mut plc = Vec::new();
        for cp in [0u32, 3, 9] {
            plc.extend_from_slice(&cp.to_le_bytes());
        }
        let pcd = |fc: u32| {
            let mut p = vec![0u8; 2];
            p.extend_from_slice(&fc.to_le_bytes());
            p.extend_from_slice(&[0, 0]);
            p
        };
        plc.extend(pcd(o1 as u32));
        plc.extend(pcd(((o2 * 2) as u32) | 0x4000_0000));
        let mut clx = vec![2u8];
        clx.extend_from_slice(&(plc.len() as u32).to_le_bytes());
        clx.extend(plc);
        word[0x01A2..0x01A6].copy_from_slice(&0u32.to_le_bytes());
        word[0x01A6..0x01AA].copy_from_slice(&(clx.len() as u32).to_le_bytes());

        let mut ole = CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        ole.create_stream("/WordDocument").unwrap().write_all(&word).unwrap();
        ole.create_stream("/1Table").unwrap().write_all(&clx).unwrap();
        ole.flush().unwrap();
        let bytes = ole.into_inner().into_inner();

        let (format, text) = read_ole(&bytes).unwrap();
        assert_eq!(format, "Word 97-2003");
        assert_eq!(text.trim(), "한글 plain");
    }

    #[test]
    fn hwp_sections() {
        let mut units: Vec<u16> = "해강".encode_utf16().collect();
        units.push(13);
        let body: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
        let mut section = (((body.len() as u32) << 20) | 67).to_le_bytes().to_vec();
        section.extend(&body);
        let mut header = vec![0u8; 256];
        header[..17].copy_from_slice(b"HWP Document File");

        let mut ole = CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        ole.create_stream("/FileHeader").unwrap().write_all(&header).unwrap();
        ole.create_storage("/BodyText").unwrap();
        ole.create_stream("/BodyText/Section0").unwrap().write_all(&section).unwrap();
        ole.flush().unwrap();
        let bytes = ole.into_inner().into_inner();

        let (format, text) = read_ole(&bytes).unwrap();
        assert_eq!(format, "Hangul");
        assert_eq!(text.trim(), "해강");
    }
}

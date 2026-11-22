//! Kindle metadata and KF8 reconstruction through the bundled libmobi.

use std::ffi::{c_char, c_int, c_uint, c_void, CStr};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::ptr::{self, NonNull};
use std::slice;

use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

use super::{html, txt, BookText};
use crate::error::{Error, Result};
use crate::metadata::{htmlize_comments, parse_datetime, Metadata};

#[cfg(unix)]
type PathChar = c_char;
#[cfg(windows)]
type PathChar = u16;

type EmitPart = extern "C" fn(*mut c_void, *const c_char, *const u8, usize) -> c_int;
type EmitMetadata = extern "C" fn(*mut c_void, c_uint, *const u8, usize);

unsafe extern "C" {
    fn cr_mobi_open(path: *const PathChar, book: *mut *mut c_void) -> c_int;
    fn mobi_free(book: *mut c_void);
    fn cr_mobi_metadata(book: *const c_void, context: *mut c_void, emit: EmitMetadata);
    fn cr_mobi_read(
        book: *const c_void,
        epub: c_int,
        context: *mut c_void,
        emit: EmitPart,
    ) -> c_int;
}

struct KindleBook(NonNull<c_void>);

impl KindleBook {
    fn open(path: &Path) -> Result<Self> {
        let path = native_path(path)?;
        let mut book = ptr::null_mut();
        // The path is NUL-terminated and the bridge only sets book on success.
        check(unsafe { cr_mobi_open(path.as_ptr(), &mut book) })?;
        NonNull::new(book)
            .map(Self)
            .ok_or_else(|| Error::Kindle("could not allocate book handle".into()))
    }

    fn read<T>(&self, epub: bool, context: &mut T, emit: EmitPart) -> Result<()> {
        // libmobi calls the callback synchronously and borrows the handle.
        check(unsafe {
            cr_mobi_read(
                self.0.as_ptr(),
                c_int::from(epub),
                ptr::from_mut(context).cast(),
                emit,
            )
        })
    }
}

impl Drop for KindleBook {
    fn drop(&mut self) {
        // This handle is uniquely owned and was allocated by libmobi.
        unsafe { mobi_free(self.0.as_ptr()) };
    }
}

#[cfg(unix)]
fn native_path(path: &Path) -> Result<Vec<PathChar>> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    if bytes.contains(&0) {
        return Err(Error::Kindle("invalid file path".into()));
    }
    Ok(bytes
        .iter()
        .map(|&byte| byte as PathChar)
        .chain([0])
        .collect())
}

#[cfg(windows)]
fn native_path(path: &Path) -> Result<Vec<PathChar>> {
    use std::os::windows::ffi::OsStrExt;
    let units: Vec<_> = path.as_os_str().encode_wide().collect();
    if units.contains(&0) {
        return Err(Error::Kindle("invalid file path".into()));
    }
    Ok(units.into_iter().chain([0]).collect())
}

fn check(status: c_int) -> Result<()> {
    match status {
        0 => Ok(()),
        4 => Err(Error::Kindle("could not open input file".into())),
        5 => Err(Error::Kindle("the book is DRM-protected".into())),
        6 => Err(Error::Kindle("unsupported Kindle document type".into())),
        _ => Err(Error::Kindle(format!(
            "could not decode book (libmobi error {status})"
        ))),
    }
}

pub(super) fn sniff(file: &mut File, header: &[u8]) -> Option<&'static str> {
    match header.get(60..68)? {
        b"TEXtREAd" => return Some("PRC"),
        b"BOOKMOBI" => {}
        _ => return None,
    }
    // The first record's offset is stored at the start of the PalmDB record
    // table. Only a standalone KF8 header should use the .azw3 extension.
    let version = (|| {
        let mut offset = [0; 4];
        file.seek(SeekFrom::Start(78)).ok()?;
        file.read_exact(&mut offset).ok()?;
        file.seek(SeekFrom::Start(u64::from(u32::from_be_bytes(offset)) + 16))
            .ok()?;
        let mut mobi = [0; 24];
        file.read_exact(&mut mobi).ok()?;
        (mobi.starts_with(b"MOBI")).then(|| u32::from_be_bytes(mobi[20..24].try_into().unwrap()))
    })();
    Some(if version == Some(8) { "AZW3" } else { "MOBI" })
}

pub(super) fn metadata(path: &Path) -> Result<(Metadata, Option<Vec<u8>>)> {
    let book = KindleBook::open(path)?;
    let mut fields = (txt::filename_metadata(path), None);
    unsafe {
        cr_mobi_metadata(
            book.0.as_ptr(),
            ptr::from_mut(&mut fields).cast(),
            collect_metadata,
        );
    }
    Ok(fields)
}

extern "C" fn collect_metadata(context: *mut c_void, tag: c_uint, data: *const u8, len: usize) {
    // The bridge supplies a live metadata pair and bytes valid for this call.
    let (meta, cover) = unsafe { &mut *context.cast::<(Metadata, Option<Vec<u8>>)>() };
    let bytes = unsafe { slice::from_raw_parts(data, len) };
    if tag == 201 {
        *cover = Some(bytes.to_vec());
        return;
    }
    let value = String::from_utf8_lossy(bytes);
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    let unescape = || html_escape::decode_html_entities(value).into_owned();
    match tag {
        100 => meta.authors.push(unescape()),
        101 => meta.publisher = Some(unescape()),
        103 => meta.comments = Some(htmlize_comments(value)),
        104 => {
            meta.identifiers.insert("isbn".into(), value.into());
        }
        105 => meta.tags.push(unescape()),
        106 => meta.pubdate = parse_datetime(value),
        113 => {
            meta.identifiers.insert("asin".into(), value.into());
        }
        503 => meta.title = unescape(),
        524 => meta
            .languages
            .extend(value.split(';').map(|lang| lang.trim().to_owned())),
        _ => {}
    }
}

pub(super) fn text(path: &Path) -> Result<BookText> {
    let mut book = BookText::default();
    KindleBook::open(path)?.read(false, &mut book, collect_chapter)?;
    Ok(book)
}

extern "C" fn collect_chapter(
    context: *mut c_void,
    _: *const c_char,
    data: *const u8,
    len: usize,
) -> c_int {
    // In text mode libmobi emits only reconstructed reading-order markup.
    let book = unsafe { &mut *context.cast::<BookText>() };
    let markup = String::from_utf8_lossy(unsafe { slice::from_raw_parts(data, len) });
    if let Some(chapter) = html::chapter(&markup, book.chapters.len() + 1) {
        book.chapters.push(chapter);
    }
    0
}

pub(super) fn write_epub(path: &Path, output: &mut File) -> Result<()> {
    let book = KindleBook::open(path)?;
    let mut zip = ZipWriter::new(output);
    zip.start_file(
        "mimetype",
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
    )?;
    zip.write_all(b"application/epub+zip")?;
    zip.start_file("META-INF/container.xml", epub_options())?;
    zip.write_all(br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#)?;
    let mut state = EpubWriter {
        zip,
        error: None,
        has_package: false,
    };
    let read = book.read(true, &mut state, write_part);
    if let Some(error) = state.error {
        return Err(error);
    }
    read?;
    if !state.has_package {
        return Err(Error::Kindle(
            "could not reconstruct an EPUB package".into(),
        ));
    }
    state.zip.finish()?;
    Ok(())
}

struct EpubWriter<'a> {
    zip: ZipWriter<&'a mut File>,
    error: Option<Error>,
    has_package: bool,
}

fn epub_options() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated)
}

extern "C" fn write_part(
    context: *mut c_void,
    name: *const c_char,
    data: *const u8,
    len: usize,
) -> c_int {
    // The bridge generates entry names and borrows the writer for this call.
    let state = unsafe { &mut *context.cast::<EpubWriter<'_>>() };
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    let bytes = unsafe { slice::from_raw_parts(data, len) };
    let result = (|| -> Result<()> {
        state.zip.start_file(name.as_ref(), epub_options())?;
        state.zip.write_all(bytes)?;
        state.has_package |= name == "OEBPS/content.opf";
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            state.error = Some(error);
            1
        }
    }
}

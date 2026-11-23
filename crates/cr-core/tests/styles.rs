//! The reader follows the book's stylesheet.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use cr_core::formats::css::Align;
use cr_core::formats::document::{write_epub, Document, Image, Section};
use cr_core::formats::reading::{BlockKind, Book, Style};
use cr_core::formats::{convert, read_document};
use cr_core::library::Library;
use cr_core::server::{Server, ServerOptions};

#[test]
fn stylesheet_shapes_blocks_and_text() {
    let document = Document {
        css: "@font-face { font-family: 'Book Font'; src: url(\"fonts/font1.ttf\") }\n\
              body { font-family: \"Book Font\", serif }\n\
              p { text-indent: 1.5em; margin: 0; text-align: justify }\n\
              p.first { text-indent: 0 }\n\
              .title { text-align: center; font-size: 140%; margin: 2em 0 1em }\n\
              .strong { font-weight: bold } .u { text-decoration: underline }\n\
              .quote { margin-left: 2em; font-style: italic }\n\
              .hidden { display: none }"
            .to_string(),
        fonts: vec![Image {
            name: "font1.ttf".into(),
            data: vec![1, 2, 3],
        }],
        sections: vec![Section {
            title: "One".into(),
            html: "<div class=\"body\"><h1 class=\"title\">One</h1>\
                   <p class=\"first\">First <span class=\"strong\">bold <span class=\"u\">words</span></span> here.</p>\
                   <p style=\"text-align: right\">Right.</p>\
                   <div class=\"hidden\"><p>Not <b>shown</b>.</p></div>\
                   <div class=\"quote\"><p>Quoted.</p></div>\
                   <p>Last.</p></div>"
                .into(),
        }],
        ..Document::default()
    };
    let book = Book::from_document(document);
    assert_eq!(book.fonts.len(), 1);
    assert_eq!(book.fonts[0].family, "book font");

    let blocks = &book.sections[0].blocks;
    let texts: Vec<&str> = blocks.iter().map(|block| block.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            "One",
            "First bold words here.",
            "Right.",
            "Quoted.",
            "Last."
        ]
    );

    let title = &blocks[0];
    assert_eq!(title.kind, BlockKind::Heading(1));
    assert_eq!(title.style.align, Align::Center);
    assert_eq!(title.style.size, Some(140));
    assert_eq!(
        (title.style.space_before, title.style.space_after),
        (200, 100)
    );

    let first = &blocks[1];
    assert_eq!(
        (first.style.align, first.style.first_indent),
        (Align::Justify, 0)
    );
    assert_eq!(first.style.size, None);
    let styled: Vec<(&str, &Style)> = first
        .spans
        .iter()
        .map(|span| (&first.text[span.start..span.end], &span.style))
        .collect();
    assert!(styled.contains(&("bold words", &Style::Bold)), "{styled:?}");
    assert!(styled.contains(&("words", &Style::Underline)), "{styled:?}");

    assert_eq!(blocks[2].style.align, Align::End);
    assert_eq!(blocks[2].style.first_indent, 150);

    let quoted = &blocks[3];
    assert_eq!(quoted.style.margin_left, 200);
    assert!(quoted
        .spans
        .iter()
        .any(|span| span.style == Style::Italic && span.end == quoted.text.len()));

    let last = &blocks[4];
    assert_eq!(last.style.margin_left, 0);
    assert!(last.spans.is_empty());
    // Every block is set in the embedded font the body asks for.
    assert!(blocks
        .iter()
        .all(|block| block.style.family.as_deref() == Some("book font")));
}

const BOOK_ID: &str = "urn:uuid:12345678-1234-1234-1234-123456789abc";
/// SHA-1 of `BOOK_ID`, the key the EPUB standard scrambles fonts with.
const BOOK_KEY: [u8; 20] = [
    0xc1, 0x2d, 0x11, 0x49, 0x54, 0x01, 0xcf, 0x12, 0x25, 0x6a, 0x83, 0x0e, 0xcd, 0xe8, 0xa7, 0x8b,
    0x17, 0x87, 0x9c, 0xc3,
];

fn font() -> Vec<u8> {
    let mut font = vec![0, 1, 0, 0];
    font.extend((0..3000u32).map(|index| (index * 7 % 251) as u8));
    font
}

fn write_styled_epub(path: &Path) {
    let mut scrambled = font();
    for (index, byte) in scrambled.iter_mut().take(1040).enumerate() {
        *byte ^= BOOK_KEY[index % 20];
    }
    let opf = format!(
        "<?xml version=\"1.0\"?><package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"bid\">\
<metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:identifier id=\"bid\"> {BOOK_ID} </dc:identifier>\
<dc:title>Styled</dc:title><dc:language>en</dc:language></metadata><manifest>\
<item id=\"c\" href=\"Text/c.xhtml\" media-type=\"application/xhtml+xml\"/>\
<item id=\"s\" href=\"Styles/s.css\" media-type=\"text/css\"/>\
<item id=\"f\" href=\"Fonts/Book%20Font.ttf\" media-type=\"font/ttf\"/></manifest>\
<spine><itemref idref=\"c\"/></spine></package>"
    );
    let entries: [(&str, &[u8]); 7] = [
        ("mimetype", b"application/epub+zip"),
        (
            "META-INF/container.xml",
            b"<?xml version=\"1.0\"?><container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\"><rootfiles><rootfile full-path=\"OEBPS/content.opf\" media-type=\"application/oebps-package+xml\"/></rootfiles></container>",
        ),
        (
            "META-INF/encryption.xml",
            b"<?xml version=\"1.0\"?><encryption xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\" xmlns:enc=\"http://www.w3.org/2001/04/xmlenc#\"><enc:EncryptedData><enc:EncryptionMethod Algorithm=\"http://www.idpf.org/2008/embedding\"/><enc:CipherData><enc:CipherReference URI=\"OEBPS/Fonts/Book%20Font.ttf\"/></enc:CipherData></enc:EncryptedData></encryption>",
        ),
        ("OEBPS/content.opf", opf.as_bytes()),
        (
            "OEBPS/Styles/s.css",
            b"@import url(http://example.com/more.css);\n\
@font-face { font-family: \"Book Font\"; src: url(\"../Fonts/Book%20Font.ttf\"); }\n\
body { font-family: \"Book Font\", serif; background: url(http://example.com/track.png); }\n\
p.note { text-align: center; }\n\
.x::after { content: \"</style><script>steal()</script>\"; }\n",
        ),
        (
            "OEBPS/Text/c.xhtml",
            b"<?xml version=\"1.0\"?><html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title>c</title><link rel=\"stylesheet\" href=\"../Styles/s.css\"/></head><body><h1>One</h1><p class=\"note\" style=\"margin-left: 2em\">Centred.</p><p style=\"background: url(http://example.com/a.png)\">Plain.</p></body></html>",
        ),
        ("OEBPS/Fonts/Book Font.ttf", &scrambled),
    ];
    let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, data) in entries {
        zip.start_file(name, stored).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
}

fn entry(archive: &Path, name: &str) -> Vec<u8> {
    let mut zip = zip::ZipArchive::new(File::open(archive).unwrap()).unwrap();
    let mut data = Vec::new();
    zip.by_name(name).unwrap().read_to_end(&mut data).unwrap();
    data
}

#[test]
fn embedded_fonts_and_styles_survive_conversion_and_reach_the_reader() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("styled.epub");
    write_styled_epub(&source);

    // The font is unscrambled, renamed and still referenced.
    let epub = directory.path().join("out.epub");
    let document = read_document(&source, true).unwrap();
    write_epub(&document, &mut File::create(&epub).unwrap()).unwrap();
    assert_eq!(entry(&epub, "OEBPS/fonts/font1.ttf"), font());
    let opf = String::from_utf8(entry(&epub, "OEBPS/content.opf")).unwrap();
    assert!(opf.contains("href=\"fonts/font1.ttf\" media-type=\"font/ttf\""));
    let css = String::from_utf8(entry(&epub, "OEBPS/style.css")).unwrap();
    assert!(css.contains("url(\"fonts/font1.ttf\")"), "{css}");
    assert!(!css.contains("example.com"), "{css}");
    let names: Vec<String> = {
        let zip = zip::ZipArchive::new(File::open(&epub).unwrap()).unwrap();
        zip.file_names().map(str::to_string).collect()
    };
    let page = names.iter().find(|name| name.contains("section")).unwrap();
    let page = String::from_utf8(entry(&epub, page)).unwrap();
    assert!(page.contains("style=\"margin-left: 2em\""), "{page}");
    assert!(!page.contains("example.com"), "{page}");

    let htmlz = directory.path().join("out.htmlz");
    convert(&source, &htmlz).unwrap();
    assert_eq!(entry(&htmlz, "fonts/font1.ttf"), font());

    // A converted book reads the same way as its source.
    for file in [&source, &epub] {
        let book = Book::open(file).unwrap();
        assert_eq!(book.fonts.len(), 1);
        assert_eq!(book.fonts[0].data, font());
        let blocks = &book.sections[0].blocks;
        let note = blocks
            .iter()
            .find(|block| block.text == "Centred.")
            .unwrap();
        assert_eq!(note.style.align, Align::Center);
        assert_eq!(note.style.margin_left, 200);
        assert_eq!(note.style.family.as_deref(), Some("book font"));
    }

    // The browser reader gets the styles and the font, and nothing more.
    let mut library = Library::create(&directory.path().join("library")).unwrap();
    let id = library.add_book(&source).unwrap().id;
    drop(library);
    let server = Server::bind(
        &directory.path().join("library"),
        &ServerOptions {
            bind: "127.0.0.1:0".into(),
            credentials: None,
        },
    )
    .unwrap();
    let page = server.respond(&format!("/read/{id}/"), None);
    let policy = &page
        .headers
        .iter()
        .find(|(name, _)| name == "Content-Security-Policy")
        .unwrap()
        .1;
    assert!(policy.contains("font-src 'self'"));
    let body = String::from_utf8(page.bytes().unwrap()).unwrap();
    assert!(body.contains("p.note { text-align: center; }"));
    assert!(!body.contains("</style><script>steal()"));
    let served = server.respond(&format!("/read/{id}/fonts/font1.ttf"), None);
    assert_eq!(
        (served.status, served.content_type.as_str()),
        (200, "font/ttf")
    );
    assert_eq!(served.bytes().unwrap(), font());
    assert_eq!(
        server
            .respond(&format!("/read/{id}/fonts/other.ttf"), None)
            .status,
        404
    );
}

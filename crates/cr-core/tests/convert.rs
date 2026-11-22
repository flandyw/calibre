//! Conversion fixtures: every input format through the document model.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;

use cr_core::formats::{convert, convert_with, extract_text, ConvertOptions, Profile};
use cr_core::jobs::JobContext;
use cr_core::library::{ConvertOutcome, Library, LibraryConvertOptions};
use tempfile::tempdir;

fn entry(archive: &Path, name: &str) -> String {
    let mut zip = zip::ZipArchive::new(File::open(archive).unwrap()).unwrap();
    let mut text = String::new();
    zip.by_name(name)
        .unwrap_or_else(|_| panic!("{name} is missing"))
        .read_to_string(&mut text)
        .unwrap();
    text
}

fn names(archive: &Path) -> Vec<String> {
    let zip = zip::ZipArchive::new(File::open(archive).unwrap()).unwrap();
    zip.file_names().map(str::to_string).collect()
}

/// Every XHTML and XML file of an EPUB must parse.
fn assert_well_formed(epub: &Path) {
    for name in names(epub) {
        if name.ends_with(".xhtml") || name.ends_with(".opf") || name.ends_with(".ncx") {
            let text = entry(epub, &name);
            cr_core::xml::parse(text.as_bytes())
                .unwrap_or_else(|error| panic!("{name}: {error}\n{text}"));
        }
    }
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(width, height, image::Rgb([200, 30, 30]));
    let mut data = Vec::new();
    image::DynamicImage::ImageRgb8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut data),
            image::ImageFormat::Png,
        )
        .unwrap();
    data
}

const HTML: &str = r##"<html><head><title>Html Book</title>
<meta name="author" content="Ann Author"><style>.x{color:red}</style><script>bad()</script></head>
<body><h1>One</h1><p class=x>First &amp; <b>bold <i>nested</b> text</i><p>Unclosed<br>line
<img src=pic.png><img src="../secret.png"><a href="#two">go</a> <a href="file:///etc/passwd">bad</a>
<h2 id=two>Two</h2><ul><li>a<li>b</ul><table><tr><td>1<td>2<tr><td>3</table>
<p dir="rtl">שלום 日本語 &nbsp;&copy; 1 < 2</p></body></html>"##;

#[test]
fn html_becomes_a_well_formed_epub() {
    let temp = tempdir().unwrap();
    let directory = temp.path().join("page");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("book.html"), HTML).unwrap();
    fs::write(directory.join("pic.png"), png(40, 20)).unwrap();
    fs::write(temp.path().join("secret.png"), png(1, 1)).unwrap();
    let epub = temp.path().join("book.epub");
    convert(&directory.join("book.html"), &epub).unwrap();
    assert_well_formed(&epub);

    let first = entry(&epub, "OEBPS/section0001.xhtml");
    assert!(first.contains("<h1>One</h1>"));
    assert!(first.contains(r#"<p class="x">First &amp; <b>bold <i>nested</i></b> text</p>"#));
    assert!(first.contains("Unclosed<br/>line"));
    assert!(first.contains(r#"<img src="images/image1.png" alt=""/>"#));
    // The link target is in the next file; unsafe links lose their target.
    assert!(first.contains(r##"<a href="section0002.xhtml#two">go</a>"##));
    assert!(first.contains("<a>bad</a>"));
    assert!(!first.contains("bad()"));

    let second = entry(&epub, "OEBPS/section0002.xhtml");
    assert!(second.contains(r#"<h2 id="two">Two</h2><ul><li>a</li><li>b</li></ul>"#));
    assert!(second.contains("<table><tr><td>1</td><td>2</td></tr><tr><td>3</td></tr></table>"));
    assert!(second.contains(r#"<p dir="rtl">שלום 日本語"#));
    assert!(second.contains("1 &lt; 2"));

    // Only the image beside the page is embedded.
    let images: Vec<String> = names(&epub)
        .into_iter()
        .filter(|name| name.starts_with("OEBPS/images/"))
        .collect();
    assert_eq!(images, ["OEBPS/images/image1.png"]);
    assert!(entry(&epub, "OEBPS/style.css").contains(".x{color:red}"));
    let opf = entry(&epub, "OEBPS/content.opf");
    assert!(opf.contains("<dc:title>Html Book</dc:title>"));
    assert!(opf.contains("<dc:creator>Ann Author</dc:creator>"));
    let nav = entry(&epub, "OEBPS/nav.xhtml");
    assert!(nav.contains(r#"<a href="section0001.xhtml">One</a>"#));
    assert!(nav.contains(r#"<a href="section0002.xhtml">Two</a>"#));

    // The EPUB reads back with the same chapters.
    let text = extract_text(&epub, None).unwrap();
    let titles: Vec<&str> = text.chapters.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(titles, ["One", "Two"]);
}

#[test]
fn text_is_split_into_chapters_and_paragraphs() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("Plain Book.txt");
    fs::write(
        &source,
        "Preface\n\nHello there\nwrapped line.\n\nChapter 1\n\nOne & <one>.\n\nChapter 2\n\nTwo.\n",
    )
    .unwrap();
    let epub = temp.path().join("book.epub");
    convert(&source, &epub).unwrap();
    assert_well_formed(&epub);
    assert!(entry(&epub, "OEBPS/section0001.xhtml").contains("<p>Hello there wrapped line.</p>"));
    assert!(entry(&epub, "OEBPS/section0002.xhtml")
        .contains("<h2>Chapter 1</h2>\n<p>One &amp; &lt;one&gt;.</p>"));
    assert!(names(&epub).contains(&"OEBPS/section0003.xhtml".to_string()));

    let single = temp.path().join("single.epub");
    let options = ConvertOptions {
        split_chapters: false,
        ..ConvertOptions::default()
    };
    convert_with(&source, &single, &options).unwrap();
    assert!(!names(&single).contains(&"OEBPS/section0002.xhtml".to_string()));
}

#[test]
fn fb2_keeps_inline_markup_images_and_notes() {
    use base64::Engine;
    let temp = tempdir().unwrap();
    let source = temp.path().join("book.fb2");
    let image = base64::engine::general_purpose::STANDARD.encode(png(8, 8));
    fs::write(
        &source,
        format!(
            r##"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" xmlns:l="http://www.w3.org/1999/xlink">
<description><title-info><author><first-name>Leo</first-name><last-name>Tolstoy</last-name></author>
<book-title>Fb Book</book-title><lang>ru</lang><coverpage><image l:href="#c.png"/></coverpage></title-info></description>
<body><section><title><p>Part One</p><p>Sub</p></title>
<p>Some <emphasis>emph</emphasis> and <strong>strong</strong><a l:href="#n1" type="note">[1]</a>.</p>
<image l:href="#c.png"/><poem><stanza><v>verse</v></stanza></poem></section>
<section><title><p>Part Two</p></title><p>Second.</p></section></body>
<body name="notes"><section id="n1"><title><p>1</p></title><p>A note.</p></section></body>
<binary id="c.png" content-type="image/png">{image}</binary></FictionBook>"##
        ),
    )
    .unwrap();
    let epub = temp.path().join("book.epub");
    convert(&source, &epub).unwrap();
    assert_well_formed(&epub);
    let first = entry(&epub, "OEBPS/section0001.xhtml");
    assert!(first.contains("<h2>Part One Sub</h2>"));
    assert!(first.contains(
        r##"Some <em>emph</em> and <strong>strong</strong><a href="section0003.xhtml#n1">[1]</a>."##
    ));
    assert!(first.contains(r#"<img src="images/image1.png" alt=""/>"#));
    assert!(entry(&epub, "OEBPS/section0003.xhtml").contains(r#"<a id="n1"></a>"#));
    let opf = entry(&epub, "OEBPS/content.opf");
    assert!(opf.contains("<dc:creator>Leo Tolstoy</dc:creator>"));
    assert!(opf.contains(r#"properties="cover-image""#));
    assert!(names(&epub).contains(&"OEBPS/titlepage.xhtml".to_string()));

    // To Kindle and back: structure and metadata survive.
    let azw3 = temp.path().join("book.azw3");
    convert(&source, &azw3).unwrap();
    let back = temp.path().join("back.epub");
    convert(&azw3, &back).unwrap();
    let text = extract_text(&back, None).unwrap().full_text();
    assert!(text.contains("Some emph and strong"));
    assert!(text.contains("A note."));
    let (metadata, cover) = cr_core::formats::epub::metadata(&back).unwrap();
    assert_eq!(metadata.title, "Fb Book");
    assert_eq!(metadata.authors, ["Leo Tolstoy"]);
    assert!(cover.is_some());
}

fn write_docx(path: &Path) {
    let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    let mut add = |name: &str, data: &[u8]| {
        zip.start_file(name, options).unwrap();
        zip.write_all(data).unwrap();
    };
    add(
        "docProps/core.xml",
        br#"<cp:coreProperties xmlns:cp="x" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Docx Book</dc:title><dc:creator>Dee Writer</dc:creator></cp:coreProperties>"#,
    );
    add(
        "word/_rels/document.xml.rels",
        br#"<Relationships><Relationship Id="rId1" Target="media/i.png"/><Relationship Id="rId2" Target="https://example.com/"/></Relationships>"#,
    );
    add("word/media/i.png", &png(3000, 1500));
    add(
        "word/document.xml",
        br#"<w:document xmlns:w="w" xmlns:r="r" xmlns:a="a"><w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Chapter A</w:t></w:r></w:p>
<w:p><w:r><w:t xml:space="preserve">Plain </w:t></w:r><w:r><w:rPr><w:b/><w:i/></w:rPr><w:t>both</w:t></w:r><w:r><w:rPr><w:b w:val="0"/></w:rPr><w:t> off &amp; on</w:t></w:r><w:hyperlink r:id="rId2"><w:r><w:t>link</w:t></w:r></w:hyperlink></w:p>
<w:p><w:pPr><w:numPr/></w:pPr><w:r><w:t>item 1</w:t></w:r></w:p><w:p><w:pPr><w:numPr/></w:pPr><w:r><w:t>item 2</w:t></w:r></w:p>
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>c1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>c2</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
<w:p><w:r><w:drawing><a:blip r:embed="rId1"/></w:drawing></w:r></w:p>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Chapter B</w:t></w:r></w:p><w:p><w:r><w:t>End.</w:t></w:r></w:p>
</w:body></w:document>"#,
    );
    zip.finish().unwrap();
}

#[test]
fn docx_keeps_structure_and_profiles_scale_images() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("book.docx");
    write_docx(&source);
    let epub = temp.path().join("book.epub");
    let options = ConvertOptions {
        profile: Profile::Kindle,
        ..ConvertOptions::default()
    };
    convert_with(&source, &epub, &options).unwrap();
    assert_well_formed(&epub);
    let first = entry(&epub, "OEBPS/section0001.xhtml");
    assert!(first.contains("<h1>Chapter A</h1>"));
    assert!(first.contains(
        r#"<p>Plain <strong><em>both</em></strong> off &amp; on<a href="https://example.com/">link</a></p>"#
    ));
    assert!(first.contains("<ul>\n<li>item 1</li>\n<li>item 2</li>\n</ul>"));
    assert!(first.contains("<td><p>c1</p>\n</td>"));
    assert!(entry(&epub, "OEBPS/section0002.xhtml").contains("<h1>Chapter B</h1>"));

    let mut zip = zip::ZipArchive::new(File::open(&epub).unwrap()).unwrap();
    let mut data = Vec::new();
    zip.by_name("OEBPS/images/image1.png")
        .unwrap()
        .read_to_end(&mut data)
        .unwrap();
    let image = image::load_from_memory(&data).unwrap();
    assert_eq!((image.width(), image.height()), (1072, 536));
}

#[test]
fn epub_converts_to_htmlz_and_text() {
    let temp = tempdir().unwrap();
    let directory = temp.path().join("page");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("book.html"), HTML).unwrap();
    fs::write(directory.join("pic.png"), png(40, 20)).unwrap();
    let epub = temp.path().join("book.epub");
    convert(&directory.join("book.html"), &epub).unwrap();

    let htmlz = temp.path().join("book.htmlz");
    convert(&epub, &htmlz).unwrap();
    let page = entry(&htmlz, "index.html");
    // Links between the EPUB's files point inside the single page again.
    assert!(page.contains(r##"<a href="#two">go</a>"##));
    assert!(page.contains(r#"<h2 id="two">Two</h2>"#));
    assert!(page.contains(r#"<img src="images/image1.png" alt=""/>"#));
    assert!(entry(&htmlz, "style.css").contains(".x{color:red}"));
    assert!(entry(&htmlz, "metadata.opf").contains("Html Book"));

    let text = temp.path().join("book.txt");
    convert(&epub, &text).unwrap();
    let text = fs::read_to_string(text).unwrap();
    assert!(text.contains("One\n\nFirst & bold nested text"));
    assert!(text.contains("1 < 2"));
}

#[test]
fn conversion_refuses_bad_requests_and_leaves_nothing_behind() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("book.txt");
    fs::write(&source, "Some text.").unwrap();
    let existing = temp.path().join("existing.epub");
    fs::write(&existing, "keep me").unwrap();
    assert!(convert(&source, &existing).is_err());
    assert_eq!(fs::read_to_string(&existing).unwrap(), "keep me");
    assert!(convert(&source, &temp.path().join("book.pdf")).is_err());
    assert!(convert(&source, &temp.path().join("copy.txt")).is_err());
    let empty = temp.path().join("empty.txt");
    fs::write(&empty, "  \n").unwrap();
    assert!(convert(&empty, &temp.path().join("empty.epub")).is_err());
    let left: Vec<_> = fs::read_dir(temp.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| !["book.txt", "existing.epub", "empty.txt"].contains(&name.as_str()))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn library_conversion_uses_library_metadata_and_keeps_the_original() {
    let temp = tempdir().unwrap();
    let mut library = Library::create(&temp.path().join("library")).unwrap();
    let source = temp.path().join("draft.txt");
    fs::write(&source, "Chapter 1\n\nFirst version.\n").unwrap();
    let id = library.add_book(&source).unwrap().id;
    let patch = cr_core::library::BookUpdate {
        title: Some("Real Title".to_string()),
        authors: Some(vec!["Real Author".to_string()]),
        ..Default::default()
    };
    library.update_book(id, &patch).unwrap();
    let other = library.add_book(&temp.path().join("draft.txt")).unwrap().id;
    library.remove_format(other, "TXT").unwrap();

    let options = LibraryConvertOptions::default();
    let results = library.convert_books(&[id, other], "epub", &options, &JobContext::detached());
    assert_eq!(
        results[0].result.as_ref().unwrap(),
        &ConvertOutcome::Converted {
            from: "TXT".to_string()
        }
    );
    // A book without a source fails alone.
    assert!(results[1].result.is_err());
    let epub = library.format_path(id, "EPUB").unwrap().unwrap();
    let opf = entry(&epub, "OEBPS/content.opf");
    assert!(opf.contains("<dc:title>Real Title</dc:title>"));
    assert!(opf.contains("<dc:creator>Real Author</dc:creator>"));
    let first = fs::read(&epub).unwrap();

    // Converting again is refused unless asked, and then keeps the old file.
    assert_eq!(
        library.convert_book(id, "EPUB", &options).unwrap(),
        ConvertOutcome::Skipped
    );
    let overwrite = LibraryConvertOptions {
        overwrite: true,
        no_split: true,
        ..Default::default()
    };
    library.convert_book(id, "EPUB", &overwrite).unwrap();
    assert_eq!(
        library.formats(id).unwrap(),
        ["EPUB", "ORIGINAL_EPUB", "TXT"]
    );
    let original = library.format_path(id, "ORIGINAL_EPUB").unwrap().unwrap();
    assert_eq!(fs::read(original).unwrap(), first);
}

fn write_zip(path: &Path, entries: &[(&str, &str)]) {
    let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, data) in entries {
        zip.start_file(*name, stored).unwrap();
        zip.write_all(data.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

#[test]
fn epub_keeps_notes_title_pages_and_links_between_files() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("source.epub");
    let page = |body: &str| {
        format!(r#"<html xmlns="http://www.w3.org/1999/xhtml"><body>{body}</body></html>"#)
    };
    write_zip(
        &source,
        &[
            ("mimetype", "application/epub+zip"),
            (
                "META-INF/container.xml",
                r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
            ),
            (
                "content.opf",
                r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="uid">urn:uuid:11111111-2222-3333-4444-555555555555</dc:identifier><dc:identifier>isbn:9780000000002</dc:identifier><dc:title>Linked</dc:title><dc:language>en</dc:language></metadata>
<manifest><item id="t" href="title.xhtml" media-type="application/xhtml+xml"/><item id="a" href="a.xhtml" media-type="application/xhtml+xml"/><item id="b" href="b.xhtml" media-type="application/xhtml+xml"/><item id="n" href="notes.xhtml" media-type="application/xhtml+xml"/></manifest>
<spine><itemref idref="t"/><itemref idref="a"/><itemref idref="b"/><itemref idref="n" linear="no"/></spine></package>"#,
            ),
            ("title.xhtml", &page("<h1>Linked</h1>")),
            (
                "a.xhtml",
                &page(
                    r##"<h2 id="start">A</h2><p>Text A <a href="b.xhtml#start">to B</a> <a href="#start">top</a> <a href="notes.xhtml#n1">1</a></p>"##,
                ),
            ),
            (
                "b.xhtml",
                &page(r##"<h2 id="start">B</h2><p>Text B <a href="a.xhtml#start">to A</a></p>"##),
            ),
            ("notes.xhtml", &page(r#"<p id="n1">The note.</p>"#)),
        ],
    );
    let output = temp.path().join("out.htmlz");
    convert(&source, &output).unwrap();
    let html = entry(&output, "index.html");
    assert!(html.contains("<h1>Linked</h1>"));
    assert!(html.contains(r#"<p id="n1">The note.</p>"#));
    assert!(html.contains(r##"<a href="#n1">1</a>"##));
    // Both files used `start`; each link still reaches its own heading.
    assert!(html.contains(r#"<h2 id="start-cr2">A</h2>"#));
    assert!(html.contains(r#"<h2 id="start-cr3">B</h2>"#));
    assert!(html.contains(r##"<a href="#start-cr3">to B</a> <a href="#start-cr2">top</a>"##));
    assert!(html.contains(r##"<a href="#start-cr2">to A</a>"##));

    // Named identifiers survive a rewrite.
    let epub = temp.path().join("out.epub");
    let options = ConvertOptions {
        profile: Profile::Kindle,
        ..ConvertOptions::default()
    };
    let metadata = cr_core::formats::epub::metadata(&source).unwrap().0;
    assert_eq!(metadata.identifiers["isbn"], "9780000000002");
    let text = temp.path().join("book.txt");
    fs::write(&text, "Some text.").unwrap();
    let with_metadata = ConvertOptions {
        metadata: Some(metadata),
        ..options
    };
    convert_with(&text, &epub, &with_metadata).unwrap();
    let written = cr_core::formats::epub::metadata(&epub).unwrap().0;
    assert_eq!(written.identifiers["isbn"], "9780000000002");
    assert_eq!(
        written.uuid.as_deref(),
        Some("11111111-2222-3333-4444-555555555555")
    );
}

#[test]
fn unclosed_embed_does_not_swallow_text_and_fb2_can_stay_whole() {
    let temp = tempdir().unwrap();
    let page = temp.path().join("page.html");
    fs::write(
        &page,
        "<h1>Only a heading</h1><embed src=x.swf><object data=y><p>After</p>",
    )
    .unwrap();
    let text = temp.path().join("page.txt");
    convert(&page, &text).unwrap();
    assert!(fs::read_to_string(&text).unwrap().contains("After"));
    let heading = temp.path().join("heading.html");
    fs::write(&heading, "<h1>Only a heading</h1>").unwrap();
    convert(&heading, &temp.path().join("heading.epub")).unwrap();

    let fb2 = temp.path().join("book.fb2");
    fs::write(
        &fb2,
        r#"<FictionBook><description><title-info><book-title>T</book-title></title-info></description><body><section><title><p>One</p></title><p>a</p></section><section><title><p>Two</p></title><p>b</p></section></body></FictionBook>"#,
    )
    .unwrap();
    let whole = temp.path().join("whole.epub");
    let options = ConvertOptions {
        split_chapters: false,
        ..ConvertOptions::default()
    };
    convert_with(&fb2, &whole, &options).unwrap();
    assert!(!names(&whole).contains(&"OEBPS/section0002.xhtml".to_string()));
    let body = entry(&whole, "OEBPS/section0001.xhtml");
    assert!(body.contains("<h2>One</h2>") && body.contains("<h2>Two</h2>"));
}

#[test]
fn docx_notes_are_kept_and_linked() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("notes.docx");
    write_zip(
        &source,
        &[
            (
                "word/document.xml",
                r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>Claim</w:t></w:r><w:r><w:footnoteReference w:id="2"/></w:r></w:p></w:body></w:document>"#,
            ),
            (
                "word/footnotes.xml",
                r#"<w:footnotes xmlns:w="w"><w:footnote w:type="separator" w:id="0"><w:p><w:r><w:t>sep</w:t></w:r></w:p></w:footnote><w:footnote w:id="2"><w:p><w:r><w:footnoteRef/><w:t>The source.</w:t></w:r></w:p></w:footnote></w:footnotes>"#,
            ),
        ],
    );
    let epub = temp.path().join("notes.epub");
    convert(&source, &epub).unwrap();
    assert_well_formed(&epub);
    let first = entry(&epub, "OEBPS/section0001.xhtml");
    assert!(first.contains(
        r##"Claim<sup><a id="cr-footnote-ref-2" href="section0002.xhtml#cr-footnote-2">1</a></sup>"##
    ));
    let notes = entry(&epub, "OEBPS/section0002.xhtml");
    assert!(notes.contains(r#"<div id="cr-footnote-2"><p>The source.</p>"#));
    assert!(notes.contains(r##"href="section0001.xhtml#cr-footnote-ref-2""##));
    assert!(!notes.contains("sep"));
}

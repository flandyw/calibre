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
                r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>Claim</w:t></w:r><w:r><w:footnoteReference w:id="3"/></w:r><w:r><w:t>Other</w:t></w:r><w:r><w:footnoteReference w:id="2"/></w:r></w:p></w:body></w:document>"#,
            ),
            (
                "word/_rels/document.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Target="https://wrong.example/"/></Relationships>"#,
            ),
            (
                "word/_rels/footnotes.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Target="https://right.example/"/></Relationships>"#,
            ),
            (
                "word/footnotes.xml",
                r#"<w:footnotes xmlns:w="w" xmlns:r="r"><w:footnote w:type="separator" w:id="0"><w:p><w:r><w:t>sep</w:t></w:r></w:p></w:footnote><w:footnote w:id="2"><w:p><w:r><w:footnoteRef/><w:t>Second.</w:t></w:r></w:p></w:footnote><w:footnote w:type="normal" w:id="3"><w:p><w:hyperlink r:id="rId1"><w:r><w:t>First.</w:t></w:r></w:hyperlink></w:p></w:footnote></w:footnotes>"#,
            ),
        ],
    );
    let epub = temp.path().join("notes.epub");
    convert(&source, &epub).unwrap();
    assert_well_formed(&epub);
    let first = entry(&epub, "OEBPS/section0001.xhtml");
    assert!(first.contains(
        r##"Claim<sup><a id="cr-footnote-ref-3" href="section0002.xhtml#cr-footnote-3">1</a></sup>"##
    ));
    let notes = entry(&epub, "OEBPS/section0002.xhtml");
    assert!(notes.contains(r#"<div id="cr-footnote-2"><p><a href="section0001.xhtml#cr-footnote-ref-2">2.</a></p><p>Second.</p>"#));
    assert!(notes.contains(r#"<a href="https://right.example/">First.</a>"#));
    assert!(!notes.contains("sep"));

    // Notes are numbered and in the order of their markers in plain text.
    let text = temp.path().join("notes.txt");
    convert(&source, &text).unwrap();
    let text = fs::read_to_string(text).unwrap();
    let (one, two) = (text.find("1.").unwrap(), text.find("2.").unwrap());
    assert!(one < text.find("First.").unwrap() && text.find("First.").unwrap() < two);
    assert!(two < text.find("Second.").unwrap());
}

/// A two-page PDF: page one has compressed text and a JPEG, page two plain
/// text, and the outline names both.
fn write_pdf(path: &Path) {
    use flate2::{write::ZlibEncoder, Compression};
    let mut jpeg = Vec::new();
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        40,
        30,
        image::Rgb([20, 90, 200]),
    ))
    .write_to(
        &mut std::io::Cursor::new(&mut jpeg),
        image::ImageFormat::Jpeg,
    )
    .unwrap();
    let first = "BT /F1 20 Tf 72 720 Td (Opening) Tj ET\n\
BT /F1 11 Tf 72 690 Td 14 TL (The quick brown fox jumps over) Tj T* (the lazy dog, twice.) Tj\n\
0 -28 Td [(A sec) -20 (ond para) 15 (graph \\(here\\).)] TJ ET\n\
q 200 0 0 150 72 400 cm /Im1 Do Q\n";
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(first.as_bytes()).unwrap();
    let first = encoder.finish().unwrap();
    let second = b"BT /F1 20 Tf 72 720 Td (Closing) Tj ET\nBT /F1 11 Tf 72 690 Td <4C61737420776F7264732E> Tj ET\n";

    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut object = |number: u32, dictionary: &str, stream: Option<&[u8]>| {
        pdf.extend(format!("{number} 0 obj\n{dictionary}\n").bytes());
        if let Some(stream) = stream {
            pdf.extend(b"stream\n");
            pdf.extend(stream);
            pdf.extend(b"\nendstream\n");
        }
        pdf.extend(b"endobj\n");
    };
    object(1, "<< /Type /Catalog /Pages 2 0 R /Outlines 9 0 R >>", None);
    object(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>", None);
    object(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R /Resources << /Font << /F1 7 0 R >> /XObject << /Im1 8 0 R >> >> >>",
        None,
    );
    object(
        4,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R /Resources << /Font << /F1 7 0 R >> >> >>",
        None,
    );
    object(
        5,
        &format!("<< /Length {} /Filter /FlateDecode >>", first.len()),
        Some(&first),
    );
    object(6, &format!("<< /Length {} >>", second.len()), Some(second));
    object(
        7,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        None,
    );
    object(
        8,
        &format!(
            "<< /Type /XObject /Subtype /Image /Width 120 /Height 100 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>",
            jpeg.len()
        ),
        Some(&jpeg),
    );
    object(
        9,
        "<< /Type /Outlines /First 10 0 R /Last 11 0 R /Count 2 >>",
        None,
    );
    object(
        10,
        "<< /Title (Opening) /Parent 9 0 R /Next 11 0 R /Dest [3 0 R /Fit] >>",
        None,
    );
    object(
        11,
        "<< /Title (Closing) /Parent 9 0 R /Prev 10 0 R /Dest [4 0 R /Fit] >>",
        None,
    );
    object(12, "<< /Title (Pdf Book) /Author (Pat Writer) >>", None);
    pdf.extend(b"trailer\n<< /Size 13 /Root 1 0 R /Info 12 0 R >>\n%%EOF\n");
    fs::write(path, pdf).unwrap();
}

#[test]
fn pdf_text_images_and_outline_become_a_book() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("book.pdf");
    write_pdf(&source);

    let text = cr_core::formats::extract_text(&source, None)
        .unwrap()
        .full_text();
    // Lines of one paragraph are joined; a gap starts a new one.
    assert!(
        text.contains("The quick brown fox jumps over the lazy dog, twice."),
        "{text}"
    );
    assert!(text.contains("A second paragraph (here)."), "{text}");
    assert!(text.contains("Last words."), "{text}");

    let document = cr_core::formats::read_document(&source, true).unwrap();
    let titles: Vec<&str> = document
        .sections
        .iter()
        .map(|section| section.title.as_str())
        .collect();
    assert_eq!(titles, ["Opening", "Closing"]);
    assert_eq!(document.images.len(), 1);
    assert!(document.images[0].data.starts_with(&[0xff, 0xd8]));

    let output = temp.path().join("book.epub");
    convert(&source, &output).unwrap();
    assert_well_formed(&output);
    assert!(names(&output)
        .iter()
        .any(|name| name.starts_with("OEBPS/images/") && name.ends_with(".jpg")));

    // A protected file says so instead of producing an empty book.
    let locked = temp.path().join("locked.pdf");
    let mut data = fs::read(&source).unwrap();
    let at = data
        .windows(7)
        .position(|window| window == b"trailer")
        .unwrap();
    data.truncate(at);
    data.extend(b"13 0 obj\n<< /Filter /Standard /V 2 >>\nendobj\ntrailer\n<< /Root 1 0 R /Encrypt 13 0 R >>\n");
    fs::write(&locked, data).unwrap();
    let refused = convert(&locked, &temp.path().join("locked.epub")).unwrap_err();
    assert!(refused.to_string().contains("encrypted"), "{refused}");
    assert!(!temp.path().join("locked.epub").exists());
}

#[test]
fn comics_keep_their_pages_in_reading_order() {
    let temp = tempdir().unwrap();
    let page = png(8, 12);
    let source = temp.path().join("comic.cbz");
    let mut zip = zip::ZipWriter::new(File::create(&source).unwrap());
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for name in [
        "pages/page10.png",
        "pages/page2.png",
        "__MACOSX/pages/._page2.png",
        "pages/page1.png",
        "notes.txt",
    ] {
        zip.start_file(name, stored).unwrap();
        zip.write_all(&page).unwrap();
    }
    zip.finish().unwrap();

    let document = cr_core::formats::read_document(&source, true).unwrap();
    assert_eq!(document.sections.len(), 3);
    assert_eq!(document.images.len(), 3);
    let order: Vec<usize> = document
        .sections
        .iter()
        .map(|section| {
            document
                .images
                .iter()
                .position(|image| section.html.contains(&image.name))
                .unwrap()
        })
        .collect();
    assert_eq!(order, [0, 1, 2]);

    let output = temp.path().join("comic.epub");
    convert(&source, &output).unwrap();
    assert_well_formed(&output);

    // A CBR that is really a ZIP is read; a RAR archive is named as such.
    let zipped = temp.path().join("zipped.cbr");
    fs::copy(&source, &zipped).unwrap();
    assert_eq!(
        cr_core::formats::read_document(&zipped, true)
            .unwrap()
            .sections
            .len(),
        3
    );
    let rar = temp.path().join("real.cbr");
    fs::write(&rar, b"Rar!\x1a\x07\x00rest").unwrap();
    let refused = cr_core::formats::read_document(&rar, true).unwrap_err();
    assert!(refused.to_string().contains("RAR"), "{refused}");
}

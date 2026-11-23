//! The reader follows the book's stylesheet.

use cr_core::formats::css::Align;
use cr_core::formats::document::{Document, Image, Section};
use cr_core::formats::reading::{BlockKind, Book, Style};

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

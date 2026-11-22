mod app;

use gpui_kit::*;

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            // Required before any gpui-kit component is used.
            gpui_kit::init(cx);
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| app::CalibreApp::new(window, cx))
            })
            .expect("failed to open the calibre-rust window");
        });
}

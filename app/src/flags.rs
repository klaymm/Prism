use std::cell::RefCell;
use std::collections::HashMap;

mod table {
    include!(concat!(env!("OUT_DIR"), "/flags.rs"));
}

thread_local!(static CACHE: RefCell<HashMap<String, slint::Image>> = RefCell::new(HashMap::new()));

pub fn image(code: Option<&str>) -> slint::Image {
    let Some(code) = code.map(str::to_ascii_lowercase) else { return slint::Image::default() };
    CACHE.with_borrow_mut(|cache| {
        cache
            .entry(code)
            .or_insert_with_key(|code| {
                table::svg(code).and_then(|svg| slint::Image::load_from_svg_data(svg).ok()).unwrap_or_default()
            })
            .clone()
    })
}

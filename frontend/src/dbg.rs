//! TEMPORARY debug sink: mirrors messages to the console and to a local
//! collector at 127.0.0.1:9876 so they can be read without devtools.

use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = r#"
export function dbg_send(s) {
    try {
        new Image().src = "http://127.0.0.1:9876/" + encodeURIComponent(s) + "?t=" + Date.now();
    } catch (e) {}
    console.log(s);
}
"#)]
extern "C" {
    pub fn dbg_send(s: &str);
}

pub fn log(s: String) {
    dbg_send(&s);
}

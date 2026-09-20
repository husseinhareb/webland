//! The desktop background, chosen by whoever is looking at it.
//!
//! The wallpaper belongs to the browser rather than to the compositor: it is
//! the one piece of the desktop that is about the machine in front of the user,
//! not the machine running the applications, and a picture that had to live on
//! the server would be the wrong picture whenever those are two machines.
//!
//! So the file never leaves the browser. It is read with `FileReader`, kept as
//! a data URL in `localStorage`, `local`, not `session`, because a wallpaper
//! is a decision made once, and applied as a CSS background.
//!
//! ponytail: a data URL in `localStorage`, which most browsers cap somewhere
//! around 5 MB. A photograph straight off a camera will not fit, and the user
//! is told so rather than left wondering. Downscale it in a canvas before
//! storing if that turns out to be a real complaint.

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{FileReader, HtmlInputElement};

use crate::scene::Scene;

const KEY: &str = "webland.wallpaper";

/// The wallpaper this browser last chose.
#[must_use]
pub fn load() -> Option<String> {
    storage()?.get_item(KEY).ok().flatten()
}

/// The `style` attribute for the desktop element.
///
/// Empty when there is no wallpaper, which leaves the stylesheet's gradients in
/// place rather than replacing them with a blank colour.
#[must_use]
pub fn style(wallpaper: Option<&String>) -> String {
    wallpaper.map_or_else(String::new, |url| {
        format!(
            "background-image: url({url}); background-size: cover; \
             background-position: center; background-repeat: no-repeat;"
        )
    })
}

/// Read the file the user picked, remember it, and show it.
pub fn chosen(input: &HtmlInputElement, scene: StoredValue<Scene, LocalStorage>) {
    let Some(file) = input.files().and_then(|files| files.get(0)) else {
        return;
    };
    let Ok(reader) = FileReader::new() else {
        return;
    };
    let finished = reader.clone();
    let done = Closure::<dyn FnMut()>::new(move || {
        let Some(url) = finished.result().ok().and_then(|value| value.as_string()) else {
            return;
        };
        let stored = storage().is_some_and(|storage| storage.set_item(KEY, &url).is_ok());
        scene.with_value(|scene| {
            scene.wallpaper.set(Some(url.clone()));
            if !stored {
                scene.show_toast(
                    String::from("Wallpaper set for now"),
                    Some(String::from("too large to remember; it will go on reload")),
                );
            }
        });
    });
    reader.set_onload(Some(done.as_ref().unchecked_ref()));
    done.forget();
    let _ = reader.read_as_data_url(&file);
}

/// Back to the stylesheet's gradients.
pub fn clear(scene: StoredValue<Scene, LocalStorage>) {
    if let Some(storage) = storage() {
        let _ = storage.remove_item(KEY);
    }
    scene.with_value(|scene| scene.wallpaper.set(None));
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

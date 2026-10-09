//! The desktop background, chosen by the user and saved on the server.
//!
//! Stored on the server at `$XDG_CONFIG_HOME/webland/wallpaper` and fetched via
//! `/api/wallpaper`. Changes are broadcast to all connected sessions via
//! `ServerMessage::Wallpaper`.

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlInputElement;

use crate::scene::Scene;

/// The wallpaper this browser last chose (loaded from server on connect).
#[must_use]
pub fn load() -> Option<String> {
    None
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

/// Read the file the user picked, upload it to the server, and notify the scene.
pub fn chosen(input: &HtmlInputElement, scene: StoredValue<Scene, LocalStorage>) {
    let Some(file) = input.files().and_then(|files| files.get(0)) else {
        return;
    };

    wasm_bindgen_futures::spawn_local(async move {
        match upload(&file).await {
            Ok(()) => {
                scene.with_value(|s| {
                    s.show_toast(String::from("Wallpaper updated"), None);
                });
            }
            Err(_) => {
                scene.with_value(|s| {
                    s.show_toast(
                        String::from("Failed to set wallpaper"),
                        Some(String::from("could not upload image to server")),
                    );
                });
            }
        }
    });
}

/// Back to the stylesheet's gradients on the server and desktop.
pub fn clear(scene: StoredValue<Scene, LocalStorage>) {
    wasm_bindgen_futures::spawn_local(async move {
        if delete().await.is_err() {
            scene.with_value(|s| {
                s.show_toast(
                    String::from("Failed to clear wallpaper"),
                    Some(String::from("server error")),
                );
            });
        }
    });
}

async fn upload(file: &web_sys::File) -> Result<(), wasm_bindgen::JsValue> {
    let window = web_sys::window().ok_or_else(|| wasm_bindgen::JsValue::from_str("no window"))?;
    let init = web_sys::RequestInit::new();
    init.set_method("POST");
    init.set_body(file);

    let headers = web_sys::Headers::new()?;
    let file_type = file.type_();
    if !file_type.is_empty() {
        headers.set("Content-Type", &file_type)?;
    }
    init.set_headers(&headers);

    let request = web_sys::Request::new_with_str_and_init("/api/wallpaper", &init)?;
    let resp_value =
        wasm_bindgen_futures::JsFuture::from(window.fetch_with_request(&request)).await?;
    let resp: web_sys::Response = resp_value.dyn_into()?;
    if !resp.ok() {
        return Err(wasm_bindgen::JsValue::from_str("upload failed"));
    }
    Ok(())
}

async fn delete() -> Result<(), wasm_bindgen::JsValue> {
    let window = web_sys::window().ok_or_else(|| wasm_bindgen::JsValue::from_str("no window"))?;
    let init = web_sys::RequestInit::new();
    init.set_method("DELETE");

    let request = web_sys::Request::new_with_str_and_init("/api/wallpaper", &init)?;
    let resp_value =
        wasm_bindgen_futures::JsFuture::from(window.fetch_with_request(&request)).await?;
    let resp: web_sys::Response = resp_value.dyn_into()?;
    if !resp.ok() {
        return Err(wasm_bindgen::JsValue::from_str("delete failed"));
    }
    Ok(())
}

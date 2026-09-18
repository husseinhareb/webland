//! The application launcher: a search box over what the compositor found
//! installed, and one button per match.

use std::rc::Rc;

use leptos::html::Input;
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use webland_protocol::{ClientMessage, encode};

use crate::protocol::WebSocketTransport;
use crate::scene::Scene;

/// How many matches are worth drawing. Nobody scrolls a launcher.
const SHOWN: usize = 40;

#[component]
pub fn Launcher(
    open: RwSignal<bool>,
    scene: StoredValue<Scene, LocalStorage>,
    transport: StoredValue<Option<Rc<WebSocketTransport>>, LocalStorage>,
) -> impl IntoView {
    let applications = scene.with_value(|scene| scene.applications);
    let filter = RwSignal::new(String::new());
    let selected = RwSignal::new(0usize);
    let search_ref: NodeRef<Input> = NodeRef::new();

    Effect::new(move |_| {
        if open.get()
            && let Some(input) = search_ref.get()
        {
            let _ = input.focus();
        }
    });

    // A plain substring match. A launcher people type two letters into does not
    // need fuzzy ranking to be useful, and the list is short.
    let matching = move || {
        let needle = filter.get().to_lowercase();
        applications
            .get()
            .into_iter()
            .filter(|app| needle.is_empty() || app.name.to_lowercase().contains(&needle))
            .take(SHOWN)
            .collect::<Vec<_>>()
    };

    let launch = move |id: u32| {
        open.set(false);
        filter.set(String::new());
        selected.set(0);
        let name = applications.with_untracked(|apps| {
            apps.iter()
                .find(|a| a.id == id)
                .map_or_else(|| String::from("Application"), |a| a.name.clone())
        });
        scene.with_value(|s| s.show_toast(format!("Starting {name}"), None));
        if let Some(transport) = transport.get_value()
            && let Ok(frame) = encode(&ClientMessage::Launch { id })
        {
            transport.send(&frame);
        }
    };

    let navigate = move |event: web_sys::KeyboardEvent| {
        let len = matching().len();
        match event.key().as_str() {
            "ArrowDown" if len > 0 => selected.update(|i| *i = (*i + 1).min(len - 1)),
            "ArrowUp" => selected.update(|i| *i = i.saturating_sub(1)),
            "Enter" => {
                if let Some(app) = matching().get(selected.get()) {
                    launch(app.id);
                }
            }
            "Escape" => open.set(false),
            _ => return,
        }
        event.prevent_default();
    };

    view! {
        <div class="menu" class:open=move || open.get()>
            <input
                node_ref=search_ref
                class="search"
                placeholder="Search apps… (↑↓ navigate, Enter launch)"
                prop:value=move || filter.get()
                on:input=move |event| {
                    filter.set(input_value(&event));
                    selected.set(0);
                }
                on:keydown=navigate
            />
            <div class="results">
                <For each=matching key=|app| app.id let:app>
                    {
                        let (app_id, name, icon) = (app.id, app.name.clone(), app.icon.clone());
                        view! {
                            <button
                                class="app"
                                class:selected=move || {
                                    matching().get(selected.get()).is_some_and(|a| a.id == app_id)
                                }
                                on:pointerenter=move |_| {
                                    if let Some(pos) = matching().iter().position(|a| a.id == app_id) {
                                        selected.set(pos);
                                    }
                                }
                                on:pointerdown=move |_| launch(app_id)
                            >
                                <span class="icon">
                                    {icon.map(|icon| view! { <img src=icon alt="" /> })}
                                </span>
                                {name}
                            </button>
                        }
                    }
                </For>
                {move || {
                    matching().is_empty().then(|| {
                        view! { <div class="no-results">"No applications found"</div> }
                    })
                }}
            </div>
        </div>
    }
}

/// The current text of an `<input>` an event came from.
fn input_value(event: &web_sys::Event) -> String {
    event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::HtmlInputElement>().ok())
        .map(|input| input.value())
        .unwrap_or_default()
}

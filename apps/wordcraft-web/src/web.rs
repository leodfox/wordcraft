//! The browser shell: web `Services`, drag-and-drop, and the eframe web runner.

use wasm_bindgen::JsCast as _;
use wordcraft_engine::Session;
use wordcraft_ui_egui::{Inbox, Services, WordApp};

const DOC_EXTS: &[&str] = &["docx", "docm", "dotx", "odt", "rtf", "txt", "md", "html", "htm", "json"];
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp"];
const CANVAS_ID: &str = "wordcraft_canvas";
const LOADING_ID: &str = "wordcraft_loading";

pub fn start() {
    eframe::WebLogger::init(log::LevelFilter::Info).ok();
    wasm_bindgen_futures::spawn_local(async {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            log::error!("no document");
            return;
        };
        let Some(canvas) = document.get_element_by_id(CANVAS_ID).and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok()) else {
            log::error!("missing <canvas id=\"{CANVAS_ID}\">");
            return;
        };
        let mut options = eframe::WebOptions::default();
        if query().contains("webgl")
            && let eframe::egui_wgpu::WgpuSetup::CreateNew(create) = &mut options.wgpu_options.wgpu_setup
        {
            create.instance_descriptor.backends = eframe::wgpu::Backends::GL;
        }
        let result = eframe::WebRunner::new()
            .start(
                canvas,
                options,
                Box::new(move |cc| {
                    if let Some(rs) = &cc.wgpu_render_state {
                        log::info!("wordcraft-web: wgpu backend {:?}", rs.adapter.get_info().backend);
                    }
                    let inbox: Inbox = Inbox::default();
                    install_bridge(inbox.clone(), cc.egui_ctx.clone());
                    let doc = if query().contains("sample") { wordcraft_engine::sample::sample_document() } else { wordcraft_doc::Document::new() };
                    let mut app = WordApp::new(Session::new(doc), services(inbox.clone(), cc.egui_ctx.clone()));
                    app.autosave = false;
                    Ok(Box::new(WebShell { app, inbox }))
                }),
            )
            .await;
        if let Some(el) = document.get_element_by_id(LOADING_ID) {
            match result {
                Ok(()) => el.remove(),
                Err(e) => el.set_inner_html(&format!("<p>WordCraft failed to start: {e:?}</p><p>A browser with WebGPU or WebGL2 is required.</p>")),
            }
        }
    });
}

fn query() -> String {
    web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default()
}

/// Wraps the app to read dropped files asynchronously (browsers can't read them synchronously)
/// and feed them through the inbox.
struct WebShell {
    app: WordApp,
    inbox: Inbox,
}

impl eframe::App for WebShell {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let dropped = ctx.input_mut(|i| std::mem::take(&mut i.raw.dropped_files));
        for f in dropped {
            let inbox = self.inbox.clone();
            let ctx = ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let name = f.path().file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "dropped".into());
                match f.bytes_async().await {
                    Ok(bytes) => {
                        inbox.lock().unwrap_or_else(|e| e.into_inner()).push((name, bytes));
                        ctx.request_repaint();
                    }
                    Err(e) => log::error!("couldn't read dropped file {name}: {e}"),
                }
            });
        }
        self.app.logic(ctx);
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.app.raw_input_hook(raw);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.ui(ui);
    }
}

fn services(inbox: Inbox, ctx: egui::Context) -> Services {
    let open_inbox = inbox.clone();
    Services {
        open_async: Some(Box::new(move |purpose: &str| {
            let inbox = open_inbox.clone();
            let ctx = ctx.clone();
            let dialog = if purpose == "picture" {
                rfd::AsyncFileDialog::new().add_filter("Pictures", IMAGE_EXTS)
            } else {
                rfd::AsyncFileDialog::new().add_filter("Documents", DOC_EXTS)
            };
            wasm_bindgen_futures::spawn_local(async move {
                let Some(file) = dialog.pick_file().await else {
                    return;
                };
                let bytes = file.read().await;
                inbox.lock().unwrap_or_else(|e| e.into_inner()).push((file.file_name(), bytes));
                ctx.request_repaint();
            });
        })),
        // Exports ask for a name; the browser decides where the download goes.
        pick_save: Some(Box::new(|name: &str| Some(name.to_string()))),
        download: Some(Box::new(|name: &str, bytes: &[u8]| {
            if let Err(e) = download(name, bytes) {
                log::error!("download of {name} failed: {e}");
            }
        })),
        inbox: Some(inbox),
        ..Default::default()
    }
}

/// Trigger a browser download of `bytes` named after the last component of `path`.
fn download(path: &str, bytes: &[u8]) -> Result<(), String> {
    let js = |e: wasm_bindgen::JsValue| format!("{e:?}");
    let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "document".into());
    if post_saved(&name, bytes) {
        return Ok(());
    }
    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type(mime_for(&name));
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &opts).map_err(js)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(js)?;
    let a: web_sys::HtmlAnchorElement = document.create_element("a").map_err(js)?.dyn_into().map_err(|_| "not an anchor")?;
    a.set_href(&url);
    a.set_download(&name);
    a.style().set_property("display", "none").map_err(js)?;
    let body = document.body().ok_or("no body")?;
    body.append_child(&a).map_err(js)?;
    a.click();
    a.remove();
    // Revoke after the click has been dispatched; the download keeps its own reference.
    let revoke = wasm_bindgen::closure::Closure::once_into_js(move || {
        web_sys::Url::revoke_object_url(&url).ok();
    });
    window.set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), 10_000).map_err(js)?;
    Ok(())
}

fn mime_for(name: &str) -> &'static str {
    match name.rsplit('.').next().map(str::to_ascii_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("pdf") => "application/pdf",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("odt") => "application/vnd.oasis.opendocument.text",
        Some("rtf") => "application/rtf",
        Some("html") => "text/html",
        Some("md" | "txt") => "text/plain",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

// ---------------------------------------------------------------------------------------------
// Ardoise host bridge (postMessage). Protocol: see BRIDGE.md in the Ardoise repository.
//   -> parent  {type:"ready", app}                           once the editor is up
//   <- editor  {type:"open", name, bytes:ArrayBuffer}         only from window.parent at ?parentOrigin=
//   -> parent  {type:"saved", app, name, bytes:ArrayBuffer}   native-format save inside an iframe
// ---------------------------------------------------------------------------------------------
const ARDOISE_APP: &str = "word";
const NATIVE_EXTS: &[&str] = &["docx"];

fn js_err(e: wasm_bindgen::JsValue) -> String {
    format!("{e:?}")
}

/// True when this page is embedded in another window (an iframe).
fn in_iframe() -> bool {
    let Some(w) = web_sys::window() else {
        return false;
    };
    match w.parent() {
        Ok(Some(p)) => !js_sys::Object::is(&p, &w),
        _ => false,
    }
}

fn str_field(obj: &wasm_bindgen::JsValue, key: &str) -> Option<String> {
    js_sys::Reflect::get(obj, &wasm_bindgen::JsValue::from_str(key)).ok()?.as_string()
}

/// Accept only `https://host[:port]` or `http://localhost[:port]` (no path, query, fragment or userinfo).
fn valid_origin(o: &str) -> bool {
    let (rest, https) = match (o.strip_prefix("https://"), o.strip_prefix("http://")) {
        (Some(r), _) => (r, true),
        (None, Some(r)) => (r, false),
        _ => return false,
    };
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':')) {
        return false;
    }
    let (host, port) = match rest.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (rest, None),
    };
    if host.is_empty() || host.starts_with(['.', '-']) || host.ends_with(['.', '-']) || host.contains(':') {
        return false;
    }
    if let Some(p) = port
        && (p.is_empty() || p.len() > 5 || !p.chars().all(|c| c.is_ascii_digit()))
    {
        return false;
    }
    https || host == "localhost"
}

/// The embedding page's origin, from `?parentOrigin=...`. `None` disables the bridge.
fn parent_origin() -> Option<String> {
    let q = query();
    let raw = q.trim_start_matches('?').split('&').find_map(|kv| kv.strip_prefix("parentOrigin="))?;
    let decoded: String = js_sys::decode_uri_component(raw).ok()?.into();
    valid_origin(&decoded).then_some(decoded)
}

/// Post `msg` to the parent window, addressed to the validated parent origin only (never "*").
fn post_to_parent(msg: &js_sys::Object) -> Result<(), String> {
    let origin = parent_origin().ok_or("bridge disabled (no valid parentOrigin)")?;
    let w = web_sys::window().ok_or("no window")?;
    let parent = w.parent().map_err(js_err)?.ok_or("no parent window")?;
    parent.post_message(msg, &origin).map_err(js_err)
}

fn message(fields: &[(&str, wasm_bindgen::JsValue)]) -> js_sys::Object {
    let o = js_sys::Object::new();
    for (k, v) in fields {
        let _ = js_sys::Reflect::set(&o, &wasm_bindgen::JsValue::from_str(k), v);
    }
    o
}

/// Listen for `open` messages from same-origin senders and announce `ready` to the parent.
fn install_bridge(inbox: Inbox, ctx: egui::Context) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(parent_origin) = parent_origin() else {
        log::info!("bridge disabled: no valid parentOrigin in the query string");
        return;
    };
    let on_message = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |ev: web_sys::MessageEvent| {
        if ev.origin() != parent_origin {
            return;
        }
        let from_parent = match (ev.source(), web_sys::window().and_then(|w| w.parent().ok().flatten())) {
            (Some(src), Some(par)) => js_sys::Object::is(&src, &par),
            _ => false,
        };
        if !from_parent {
            return;
        }
        let data = ev.data();
        if str_field(&data, "type").as_deref() != Some("open") {
            return;
        }
        let Ok(bytes) = js_sys::Reflect::get(&data, &wasm_bindgen::JsValue::from_str("bytes")) else {
            return;
        };
        if !(bytes.is_instance_of::<js_sys::ArrayBuffer>() || bytes.is_instance_of::<js_sys::Uint8Array>()) {
            log::warn!("bridge: open message without ArrayBuffer bytes ignored");
            return;
        }
        let name = str_field(&data, "name").filter(|n| !n.is_empty()).unwrap_or_else(|| "document".into());
        let bytes = js_sys::Uint8Array::new(&bytes).to_vec();
        inbox.lock().unwrap_or_else(|e| e.into_inner()).push((name, bytes));
        ctx.request_repaint();
    });
    let _ = window.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref());
    on_message.forget();
    if in_iframe()
        && let Err(e) = post_to_parent(&message(&[("type", "ready".into()), ("app", ARDOISE_APP.into())]))
    {
        log::error!("bridge: ready message failed: {e}");
    }
}

/// Native-format save inside an iframe: hand the bytes to the host instead of downloading.
/// Returns true when the bytes were posted.
fn post_saved(name: &str, bytes: &[u8]) -> bool {
    let ext = name.rsplit('.').next().map(str::to_ascii_lowercase).unwrap_or_default();
    if !in_iframe() || parent_origin().is_none() || !NATIVE_EXTS.contains(&ext.as_str()) {
        return false;
    }
    let buffer = js_sys::Uint8Array::from(bytes).buffer();
    let msg = message(&[("type", "saved".into()), ("app", ARDOISE_APP.into()), ("name", name.into()), ("bytes", buffer.into())]);
    match post_to_parent(&msg) {
        Ok(()) => true,
        Err(e) => {
            log::error!("bridge: saved message failed ({e}); falling back to download");
            false
        }
    }
}

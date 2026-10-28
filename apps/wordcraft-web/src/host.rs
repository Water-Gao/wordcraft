//! The `postMessage` bridge to a same-origin parent page (`?host=parent`), for pages that embed
//! WordCraft in an `<iframe>` and keep its files (a Nextcloud app, a personal file server). The
//! same messages as GridCraft's and PdfCraft's bridges; each is a plain object with a `type`:
//!
//! - `wordcraft:ready` (to the parent): `{ version }`, sent once the app is running and
//!   listening; send a file now.
//! - `wordcraft:open` (from the parent): `{ name, bytes }`, bytes an `ArrayBuffer` or
//!   `Uint8Array`. It opens like a document picked with File › Open or dropped on the window.
//! - `wordcraft:save` (to the parent): `{ name, saveAs, bytes }` (a transferred `ArrayBuffer`) for
//!   every file WordCraft would otherwise download. The parent stores it and reports failures
//!   itself. `saveAs` says whether the file becomes the document's file:
//!   - Save: `false`. It writes the opened file under its own name and format, as the desktop
//!     app does. A name the parent never sent (an untitled document, an imported .doc saved as
//!     .docx) is a new file: `true`, as for Save As.
//!   - Save As, and File › Export to another document format (which runs Save As): `true` for a
//!     format that keeps everything (.docx and the other Word packages, .odt, .rtf). The document
//!     is now that file; the next Save sends its name with `false`.
//!   - Save As or Export to a format that doesn't (PDF, Markdown, plain text), Export as PDF and
//!     Export Page as PNG: `false`. They are copies; the document's file stays the same.
//! - `wordcraft:dirty` (to the parent): `{ dirty }` whenever unsaved changes appear or go away.
//! - `wordcraft:failed` (to the parent): `{ error }` when the app couldn't start.
//! - `wordcraft:execute` (from the parent): `{ id, method, params }` runs one control-protocol
//!   method (`engine.execute`, `engine.commands`, `document.inspect`, `ui.render`, `ui.parity`)
//!   against the app in-process, and answers `wordcraft:result` (to the parent):
//!   `{ id, ok, result }` or `{ id, ok: false, error }` — the web twin of the desktop control
//!   server's protocol, so an agent can drive the framed app like the desktop one.
//!
//! Only the parent window on the page's own origin is heard or answered. In host mode the parent
//! guards leaving the page, and `?author=` names the user for comments and tracked changes.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde_json::{Value, json};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;
use wordcraft_ui_egui::{Inbox, Services, WordApp};

/// The longest user name taken from `?author=`.
const MAX_AUTHOR_CHARS: usize = 200;

const NO_DOCUMENT: &str = "no document is open; create one (file.new) or open one (file.open) first";
const UNSAVED: &str = "the document has unsaved changes: save it first, or pass {\"force\": true} to discard them and close";

/// One queued `wordcraft:execute` request from the parent.
pub struct ExecRequest {
    pub id: u64,
    pub method: String,
    pub params: Value,
}

/// `wordcraft:execute` requests waiting for the app's next frame; the shell drains and answers
/// them in `logic`, so a command's effect is on screen the moment its reply is sent.
pub type ExecQueue = Rc<RefCell<Vec<ExecRequest>>>;

#[derive(Clone)]
pub struct Bridge {
    window: web_sys::Window,
    parent: web_sys::Window,
    origin: String,
    exec: ExecQueue,
}

impl Bridge {
    /// `None` when the page isn't framed (there is no parent to talk to), or when the parent is
    /// on another origin: messages only go to this origin, so the browser would drop every save
    /// without an error while the document showed as saved.
    pub fn new() -> Option<Self> {
        let window = web_sys::window()?;
        let parent = window.parent().ok().flatten()?;
        if js_sys::Object::is(&parent, &window) {
            return None;
        }
        let origin = window.location().origin().ok()?;
        // An opaque origin (a `file://` page in some browsers) can't be a `postMessage` target.
        if origin == "null" {
            return None;
        }
        // Reading another origin's location throws, which is `None` too.
        if parent.location().origin().ok()? != origin {
            return None;
        }
        Some(Self { window, parent, origin, exec: Rc::new(RefCell::new(Vec::new())) })
    }

    /// The queue the app's shell drains every frame to run and answer `wordcraft:execute`.
    pub fn exec_queue(&self) -> ExecQueue {
        self.exec.clone()
    }

    /// Hand saved and exported files to the parent instead of downloading them, report unsaved
    /// changes to it, and hear the files it sends.
    pub fn connect(&self, services: &mut Services, inbox: Inbox, ctx: egui::Context) {
        self.listen(inbox, ctx);
        let bridge = self.clone();
        services.host_save = Some(Box::new(move |name: &str, bytes: &[u8], save_as: bool| bridge.post_save(name, bytes, save_as)));
        // Anything else that would download goes to the parent too.
        let bridge = self.clone();
        services.download = Some(Box::new(move |name: &str, bytes: &[u8]| bridge.post_save(name, bytes, false)));
        // The browser's save picker is never used, so no save finishes later.
        services.async_saves = None;
        let bridge = self.clone();
        let reported: Rc<Cell<Option<bool>>> = Rc::default();
        services.on_dirty = Some(Box::new(move |dirty: bool| {
            if reported.get() != Some(dirty) && bridge.send(&[("type", "wordcraft:dirty".into()), ("dirty", dirty.into())]) {
                reported.set(Some(dirty));
            }
        }));
    }

    /// `?author=`: the host's name for the user signs comments and tracked changes.
    pub fn set_author(app: &mut WordApp, author: &str) {
        let author: String = author.trim().chars().take(MAX_AUTHOR_CHARS).collect();
        if !author.is_empty() {
            app.session.author = author;
        }
    }

    pub fn post_ready(&self) {
        let capabilities = js_sys::Array::of1(&JsValue::from_str("dual-pane"));
        self.send(&[
            ("type", "wordcraft:ready".into()),
            ("version", env!("CARGO_PKG_VERSION").into()),
            ("capabilities", capabilities.into()),
        ]);
    }

    /// The app couldn't start (no WebGPU or WebGL2, for example): let the parent say so.
    pub fn post_failed(&self, error: &str) {
        self.send(&[("type", "wordcraft:failed".into()), ("error", error.into())]);
    }

    fn post_save(&self, path: &str, bytes: &[u8], save_as: bool) -> Result<(), String> {
        let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
        let buffer = js_sys::Uint8Array::from(bytes).buffer();
        let message = message(&[
            ("type", "wordcraft:save".into()),
            ("name", name.as_str().into()),
            ("saveAs", save_as.into()),
            ("bytes", buffer.clone().into()),
        ])
        .ok_or_else(|| "couldn't build the message".to_string())?;
        self.parent
            .post_message_with_transfer(&message, &self.origin, &js_sys::Array::of1(&buffer))
            .map_err(|e| e.as_string().unwrap_or_else(|| format!("{e:?}")))
    }

    /// Answer one `wordcraft:execute`. The payload goes over as a real object (built by
    /// `JSON.parse`, so `serde_json` never has to touch `JsValue` fields one by one).
    pub fn post_result(&self, id: u64, r: &Result<Value, String>) {
        let payload = match r {
            Ok(v) => json!({"type": "wordcraft:result", "id": id, "ok": true, "result": v}),
            Err(e) => json!({"type": "wordcraft:result", "id": id, "ok": false, "error": e}),
        };
        let Ok(text) = serde_json::to_string(&payload) else { return };
        if let Ok(message) = js_sys::JSON::parse(&text) {
            let _ = self.parent.post_message(&message, &self.origin);
        }
    }

    /// Hear `wordcraft:open` (queue the file like a dropped one) and `wordcraft:execute` (queue
    /// the request for the shell to run on its next frame) from the parent.
    fn listen(&self, inbox: Inbox, ctx: egui::Context) {
        let parent = self.parent.clone();
        let origin = self.origin.clone();
        let exec = self.exec.clone();
        let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            if event.origin() != origin || !event.source().is_some_and(|s| js_sys::Object::is(&s, &parent)) {
                return;
            }
            let data = event.data();
            match string(&data, "type").as_deref() {
                Some("wordcraft:open") => {
                    let name = string(&data, "name").filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Document.docx".to_string());
                    let Some(bytes) = bytes(&data) else { return };
                    inbox.lock().unwrap_or_else(|e| e.into_inner()).push((name, bytes));
                    ctx.request_repaint();
                }
                Some("wordcraft:execute") => {
                    // `event.data` is a plain object; stringify once and let serde do the rest.
                    let Ok(text) = js_sys::JSON::stringify(&data) else { return };
                    let Some(text) = text.as_string() else { return };
                    let Ok(req) = serde_json::from_str::<Value>(&text) else { return };
                    let Some(id) = req.get("id").and_then(Value::as_u64) else { return };
                    let Some(method) = req.get("method").and_then(Value::as_str).map(str::to_string) else { return };
                    let params = req.get("params").cloned().filter(|v| !v.is_null()).unwrap_or_else(|| json!({}));
                    exec.borrow_mut().push(ExecRequest { id, method, params });
                    ctx.request_repaint();
                }
                _ => {}
            }
        });
        if self.window.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref()).is_ok() {
            // The listener lives as long as the page.
            on_message.forget();
        }
    }

    /// Post a message to the parent; false if it couldn't be sent.
    fn send(&self, fields: &[(&str, JsValue)]) -> bool {
        message(fields).is_some_and(|m| self.parent.post_message(&m, &self.origin).is_ok())
    }
}

fn message(fields: &[(&str, JsValue)]) -> Option<js_sys::Object> {
    let m = js_sys::Object::new();
    for (key, value) in fields {
        js_sys::Reflect::set(&m, &JsValue::from_str(key), value).ok()?;
    }
    Some(m)
}

fn string(data: &JsValue, key: &str) -> Option<String> {
    js_sys::Reflect::get(data, &JsValue::from_str(key)).ok()?.as_string()
}

/// `bytes` as an `ArrayBuffer` or `Uint8Array`.
fn bytes(data: &JsValue) -> Option<Vec<u8>> {
    let bytes = js_sys::Reflect::get(data, &JsValue::from_str("bytes")).ok()?;
    if let Some(array) = bytes.dyn_ref::<js_sys::Uint8Array>() {
        Some(array.to_vec())
    } else if bytes.is_instance_of::<js_sys::ArrayBuffer>() {
        Some(js_sys::Uint8Array::new(&bytes).to_vec())
    } else {
        None
    }
}

/// Flattens `with_document`'s double `Result` (no such document / the command itself failed).
fn flatten<E: std::fmt::Display, F: std::fmt::Display>(r: Result<Result<Value, E>, F>) -> Result<Value, String> {
    match r {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(e.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// Runs one `wordcraft:execute` against the app — the web twin of the desktop control server's
/// dispatch (`crates/ui-egui/src/control.rs`): same method names, same reply shapes, so one MCP
/// client works against either app.
pub fn execute(app: &mut WordApp, req: &ExecRequest) -> Result<Value, String> {
    let p = &req.params;
    if let Some(doc) = p.get("doc").and_then(Value::as_u64) {
        return execute_in(app, doc, req);
    }
    match req.method.as_str() {
        "engine.execute" | "command" => {
            let Some(id) = p.get("command").and_then(Value::as_str) else { return Err("missing `command`".into()) };
            let params = p.get("params").cloned().filter(|v| !v.is_null()).unwrap_or_else(|| json!({}));
            let result = app.execute(id, params);
            // Agent 新建/打开文档没有 Backstage 按钮的点击事件；文档已打开时必须退出开始页，
            // 否则 API 成功而画面仍停在 Home，双视口也会被开始页遮住。
            if result.is_ok() && app.has_document() && matches!(id, "file.new" | "file.open" | "view.switchWindows") {
                app.ui.backstage = false;
            }
            result
        }
        "engine.commands" => {
            let reg = app.session.registry.clone();
            let commands: Vec<Value> = reg
                .all()
                .iter()
                .map(|c| json!({"id": c.id, "label": c.label, "location": c.location, "shortcut": c.shortcut, "params": c.params, "enabled": app.session.why_disabled(c.id).is_none()}))
                .collect();
            Ok(Value::Array(commands))
        }
        "workspace.inspect" => Ok(app.workspace_snapshot()),
        "workspace.setSplit" => {
            let enabled = p.get("enabled").and_then(Value::as_bool).unwrap_or(false);
            app.set_split(enabled);
            Ok(app.workspace_snapshot())
        }
        "view.focusPane" => {
            let Some(pane) = p.get("pane").and_then(Value::as_str).and_then(wordcraft_ui_egui::workspace::Pane::parse) else {
                return Err("`pane` must be `left` or `right`".into());
            };
            app.focus_pane(pane);
            Ok(app.workspace_snapshot())
        }
        "document.inspect" | "ui.render" if !app.has_document() => Err(NO_DOCUMENT.into()),
        "document.inspect" => app.session.run("document.inspect", p).map_err(|e| e.to_string()),
        "ui.render" => {
            let page = p.get("page").and_then(Value::as_u64).unwrap_or(1);
            let scale = p.get("scale").and_then(Value::as_f64).map_or(2.0, |v| v as f32);
            wordcraft_ui_egui::control::render_png(&app.session, page, scale)
        }
        "ui.parity" => Ok(wordcraft_engine::catalog::parity(&app.session.registry)),
        other => Err(format!("unknown method `{other}`")),
    }
}

/// A `doc`-addressed request: runs on that open document while the window keeps showing the
/// active one — the web twin of `handle_in` in the desktop control server.
fn execute_in(app: &mut WordApp, doc: u64, req: &ExecRequest) -> Result<Value, String> {
    let p = &req.params;
    match req.method.as_str() {
        "document.inspect" => flatten(app.session.with_document(doc, |s| s.run("document.inspect", p))),
        "ui.render" => {
            let page = p.get("page").and_then(Value::as_u64).unwrap_or(1);
            let scale = p.get("scale").and_then(Value::as_f64).map_or(2.0, |v| v as f32);
            flatten(app.session.with_document(doc, |s| wordcraft_ui_egui::control::render_png(s, page, scale)))
        }
        _ => {
            let Some(id) = p.get("command").and_then(Value::as_str) else { return Err("missing `command`".into()) };
            let params = p.get("params").cloned().filter(|v| !v.is_null()).unwrap_or_else(|| json!({}));
            if id.starts_with("ui.") {
                return Err(format!("`{id}` works on the window: make the document active first (view.switchWindows {{\"id\": {doc}}})"));
            }
            // A background document can't save in the web app: saving hands the bytes to the host
            // page through the app's UI layer, which only runs on the active document. A direct
            // engine save here would die in the wasm file layer and take the instance with it.
            if id == "file.save" || id == "file.saveAs" || id.starts_with("file.export") {
                return Err(format!("`{id}` on a background document is not available in the web app: make it active first (view.switchWindows {{\"id\": {doc}}}), then save"));
            }
            // Close, with the same guard as the window's: unsaved changes need `force`.
            if id == "file.close" {
                let force = params.get("force").and_then(Value::as_bool) == Some(true);
                if !force && app.session.documents().iter().any(|d| d.id == doc && d.dirty) {
                    return Err(UNSAVED.into());
                }
                return app.session.close_document(doc).map_err(|e| e.to_string()).map(|()| json!({"closed": true, "id": doc, "documents": app.session.documents()}));
            }
            let r = flatten(app.session.with_document(doc, |s| s.run(id, &params)));
            // UI requests the command made carry `document`, so the window can tell they aren't
            // about the document it shows.
            for request in std::mem::take(&mut app.session.ui_requests) {
                app.handle_request(&request);
            }
            r
        }
    }
}

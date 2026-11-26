// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx

//! Vista previa de lo que escribe el modelo (una página HTML o un SVG) en una
//! ventana de Karbyx con el motor de Safari. La página se ejecuta sin red: no
//! puede cargar nada de fuera ni irse a otra dirección, así que no puede
//! enviar lo que el modelo haya visto de tus archivos.

/// Se puede ver: HTML o SVG, por el lenguaje del bloque o, sin él, por cómo empieza.
pub fn can_preview(lang: &str, text: &str) -> bool {
    match lang.to_ascii_lowercase().as_str() {
        "html" | "htm" | "xhtml" | "svg" => true,
        "" => {
            let t = start(text);
            t.starts_with("<!doctype html") || t.starts_with("<html") || is_svg(text)
        }
        _ => false,
    }
}

/// El principio del texto, en minúsculas, sin espacios delante.
fn start(text: &str) -> String {
    text.trim_start().chars().take(200).collect::<String>().to_ascii_lowercase()
}

fn is_svg(text: &str) -> bool {
    let t = start(text);
    t.starts_with("<svg") || (t.starts_with("<?xml") && t.contains("<svg"))
}

/// La página que se carga: el HTML tal cual; un SVG, centrado en una página.
pub fn page(text: &str) -> String {
    if !is_svg(text) {
        return text.to_string();
    }
    format!(
        "<!doctype html><meta charset=\"utf-8\"><style>html,body{{margin:0;height:100%}}\
         body{{display:grid;place-items:center;background:#fff}}svg{{max-width:100vw;max-height:100vh}}</style>{}",
        text.trim_start().strip_prefix("<?xml").map_or(text, |r| r.find("?>").map_or(r, |i| &r[i + 2..]))
    )
}

/// Bloquea toda carga salvo la propia página (`about:`), `data:` y `blob:`.
#[cfg(target_os = "macos")]
const RULES: &str = r#"[
  {"trigger": {"url-filter": ".*"}, "action": {"type": "block"}},
  {"trigger": {"url-filter": "^about:"}, "action": {"type": "ignore-previous-rules"}},
  {"trigger": {"url-filter": "^data:"}, "action": {"type": "ignore-previous-rules"}},
  {"trigger": {"url-filter": "^blob:"}, "action": {"type": "ignore-previous-rules"}}
]"#;

/// WebRTC no pasa por esas reglas: se quita antes de que empiece la página.
#[cfg(target_os = "macos")]
const NO_RTC: &str = "for (const k of ['RTCPeerConnection', 'webkitRTCPeerConnection', 'RTCDataChannel', \
     'RTCRtpSender', 'RTCRtpReceiver']) { try { Object.defineProperty(window, k, { value: undefined, \
     writable: false, configurable: false }); } catch (e) {} }";

#[cfg(target_os = "macos")]
mod mac {
    use std::cell::RefCell;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{MainThreadMarker, MainThreadOnly, Message};
    use objc2_app_kit::{NSBackingStoreType, NSWindow, NSWindowStyleMask};
    use objc2_foundation::{NSError, NSPoint, NSRect, NSSize, NSString, ns_string};
    use objc2_web_kit::{
        WKContentRuleList, WKContentRuleListStore, WKUserScript, WKUserScriptInjectionTime, WKWebView,
        WKWebViewConfiguration, WKWebsiteDataStore,
    };

    thread_local! {
        /// Las ventanas abiertas: si no se guardan, se cierran solas.
        static WINDOWS: RefCell<Vec<(Retained<NSWindow>, Retained<WKWebView>)>> = const { RefCell::new(Vec::new()) };
        /// Las reglas sin red, compiladas la primera vez.
        static RULES: RefCell<Option<Retained<WKContentRuleList>>> = const { RefCell::new(None) };
    }

    /// Abre una ventana con la página. Las reglas se compilan aparte: la página
    /// no se carga hasta tenerlas, y si fallan, no se carga.
    pub fn open(title: &str, html: String, on_error: impl Fn(String) + 'static) {
        let Some(mtm) = MainThreadMarker::new() else {
            on_error("la vista previa solo se abre desde la ventana".into());
            return;
        };
        if let Some(rules) = RULES.with_borrow(Clone::clone) {
            show(mtm, title, &html, &rules);
            return;
        }
        let Some(store) = (unsafe { WKContentRuleListStore::defaultStore(mtm) }) else {
            on_error("no se pudo preparar el bloqueo de la red".into());
            return;
        };
        let title = title.to_string();
        let done = RcBlock::new(move |list: *mut WKContentRuleList, err: *mut NSError| {
            // SAFETY: WebKit da una de las dos cosas, válida durante la llamada.
            match unsafe { list.as_ref() } {
                Some(list) => {
                    let list: Retained<WKContentRuleList> = list.retain();
                    RULES.set(Some(list.clone()));
                    show(mtm, &title, &html, &list);
                }
                None => {
                    let why = unsafe { err.as_ref() }
                        .map(|e| e.localizedDescription().to_string())
                        .unwrap_or_else(|| "error desconocido".into());
                    on_error(format!("no se pudo bloquear la red ({why}): no se abre"));
                }
            }
        });
        unsafe {
            store.compileContentRuleListForIdentifier_encodedContentRuleList_completionHandler(
                Some(ns_string!("karbyx-sin-red")),
                Some(&NSString::from_str(super::RULES)),
                Some(&done),
            );
        }
    }

    /// El texto que enseña la última vista previa (para las capturas).
    pub fn read_text(on_text: impl Fn(String) + 'static) {
        let Some(web) = WINDOWS.with_borrow(|w| w.last().map(|(_, web)| web.clone())) else {
            return on_text("(sin vista previa)".into());
        };
        let done = RcBlock::new(move |v: *mut AnyObject, err: *mut NSError| {
            // SAFETY: WebKit da el resultado o el error, válidos durante la llamada.
            let text =
                unsafe { v.as_ref() }.and_then(|v| v.downcast_ref::<NSString>()).map(|s| s.to_string());
            let err = unsafe { err.as_ref() }.map(|e| e.localizedDescription().to_string());
            on_text(text.or(err).unwrap_or_default());
        });
        unsafe {
            web.evaluateJavaScript_completionHandler(ns_string!("document.body.innerText"), Some(&done))
        };
    }

    fn show(mtm: MainThreadMarker, title: &str, html: &str, rules: &WKContentRuleList) {
        unsafe {
            let config = WKWebViewConfiguration::new(mtm);
            // Nada se guarda entre una vista y otra.
            config.setWebsiteDataStore(&WKWebsiteDataStore::nonPersistentDataStore(mtm));
            let content = config.userContentController();
            content.addContentRuleList(rules);
            let script = WKUserScript::initWithSource_injectionTime_forMainFrameOnly(
                WKUserScript::alloc(mtm),
                &NSString::from_str(super::NO_RTC),
                WKUserScriptInjectionTime::AtDocumentStart,
                false,
            );
            content.addUserScript(&script);

            let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(960.0, 720.0));
            let web = WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &config);
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable
                    | NSWindowStyleMask::Resizable,
                NSBackingStoreType::Buffered,
                false,
            );
            // La guardamos nosotros: que cerrarla no la libere dos veces.
            window.setReleasedWhenClosed(false);
            window.setTitle(&NSString::from_str(title));
            window.setContentView(Some(&web));
            window.center();
            web.loadHTMLString_baseURL(&NSString::from_str(html), None);
            window.makeKeyAndOrderFront(None);
            WINDOWS.with_borrow_mut(|w| {
                w.retain(|(w, _)| w.isVisible() || w.isMiniaturized());
                w.push((window, web));
            });
        }
    }
}

#[cfg(target_os = "macos")]
pub use mac::{open, read_text};

#[cfg(not(target_os = "macos"))]
pub fn read_text(on_text: impl Fn(String) + 'static) {
    on_text(String::new());
}

#[cfg(not(target_os = "macos"))]
pub fn open(_title: &str, _html: String, on_error: impl Fn(String) + 'static) {
    on_error("la vista previa solo existe en macOS".into());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_can_be_seen() {
        assert!(can_preview("html", "<div>"));
        assert!(can_preview("HTML", ""));
        assert!(can_preview("svg", "<svg/>"));
        assert!(can_preview("", "  <!DOCTYPE html>\n<html>"));
        assert!(can_preview("", "<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"/>"));
        assert!(!can_preview("", "<div>suelto</div>"));
        assert!(!can_preview("js", "<html>"));
        assert!(!can_preview("xml", "<svg/>"));
    }

    #[test]
    fn svg_goes_inside_a_page() {
        let html = "<!doctype html><title>t</title>";
        assert_eq!(page(html), html);
        let p = page("<?xml version=\"1.0\"?>\n<svg width=\"10\"/>");
        assert!(p.starts_with("<!doctype html>"), "{p}");
        assert!(p.ends_with("\n<svg width=\"10\"/>"), "{p}");
        assert!(!p.contains("<?xml"));
    }
}

//! Starts Photocraft in the browser from compressed parts of its WebAssembly.
//!
//! Static hosts cap the size of one file (Cloudflare Pages: 25 MiB), and the app's wasm is at
//! that size. `packaging/web/package.sh` gzips it and cuts it into parts listed in
//! `app/parts.txt`. This small module is what the packaged page loads: it fetches the parts in
//! order, joins and inflates them as they arrive, and hands the stream to the app's own generated
//! loader, so the browser still compiles while downloading. No file on the host is larger than
//! one part, however large the app grows.
//!
//! The browser inflates the parts (`DecompressionStream`: Safari 16.4, Firefox 113, Chrome 80).
//!
//! A download that fails is tried again, the later tries past the browser's cache (a bad copy
//! there would otherwise fail forever). When every try fails the page says so and offers a reload.
//!
//! `trunk build` in `apps/photocraft-web` still gives the app as one `.wasm`, for development and
//! for hosts without a per-file cap; only the packaged site starts through this module.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

/// First line of `parts.txt`. Some hosts answer a missing file with the home page and HTTP 200;
/// without this an HTML page would be read as a list of parts.
#[cfg(any(target_arch = "wasm32", test))]
const MARKER: &str = "photocraft-parts 1";

/// The part names in a `parts.txt`, in order, or `None` when the text isn't one. A name is a
/// file next to `parts.txt`: anything that could point elsewhere is refused.
#[cfg(any(target_arch = "wasm32", test))]
fn parts(list: &str) -> Option<Vec<String>> {
    let mut lines = list.lines().map(str::trim).filter(|l| !l.is_empty());
    if lines.next() != Some(MARKER) {
        return None;
    }
    let parts: Vec<String> = lines.map(str::to_owned).collect();
    let plain = |p: &String| !p.starts_with('.') && !p.contains(['/', '\\', '?', '#', ':', '%']);
    (!parts.is_empty() && parts.iter().all(plain)).then_some(parts)
}

#[cfg(target_arch = "wasm32")]
mod boot {
    use std::cell::RefCell;
    use std::rc::Rc;

    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::{JsFuture, spawn_local};
    use web_sys::{
        Headers, ReadableStream, ReadableWritablePair, RequestCache, RequestInit, Response, ResponseInit, StreamPipeOptions, TransformStream, WritableStream,
    };

    /// Where packaging puts the app, next to this module's own files.
    const PARTS: &str = "./app/parts.txt";
    /// Seconds to wait before each further try; one more than this many tries are made.
    const RETRY_AFTER: [u32; 2] = [1, 3];

    // The app's `wasm-bindgen` loader. Its default export instantiates the module it is given and
    // runs the app's `main`. The path is fixed at compile time, so packaging gives the file this
    // name (Trunk's own name for it carries a content hash).
    #[wasm_bindgen(raw_module = "./app/photocraft-web.js")]
    extern "C" {
        #[wasm_bindgen(js_name = default, catch)]
        async fn start_app(module: JsValue) -> Result<JsValue, JsValue>;
    }

    // The browser's own inflater. `web-sys` has it only behind its unstable-API flag, which would
    // have to be set for the whole build, so the three members used are declared here.
    #[wasm_bindgen]
    extern "C" {
        type DecompressionStream;
        #[wasm_bindgen(constructor, catch)]
        fn new(format: &str) -> Result<DecompressionStream, JsValue>;
        #[wasm_bindgen(method, getter)]
        fn readable(this: &DecompressionStream) -> ReadableStream;
        #[wasm_bindgen(method, getter)]
        fn writable(this: &DecompressionStream) -> WritableStream;
    }

    fn window() -> Result<web_sys::Window, JsValue> {
        web_sys::window().ok_or_else(|| JsValue::from_str("no window"))
    }

    /// `fresh` skips the browser's cache.
    async fn fetch(url: &str, fresh: bool) -> Result<Response, JsValue> {
        let init = RequestInit::new();
        if fresh {
            init.set_cache(RequestCache::Reload);
        }
        let response: Response = JsFuture::from(window()?.fetch_with_str_and_init(url, &init)).await?.dyn_into()?;
        if !response.ok() {
            return Err(JsValue::from_str(&format!("{url}: HTTP {}", response.status())));
        }
        if response.headers().get("Content-Type")?.is_some_and(|t| t.starts_with("text/html")) {
            return Err(JsValue::from_str(&format!("{url}: the server sent a web page, not the file")));
        }
        Ok(response)
    }

    /// Copies each part into `sink` in order, then closes it.
    async fn feed(parts: Vec<String>, sink: WritableStream, fresh: bool) -> Result<(), JsValue> {
        let keep_open = StreamPipeOptions::new();
        keep_open.set_prevent_close(true);
        for part in &parts {
            let body = fetch(&format!("./app/{part}"), fresh).await?.body().ok_or_else(|| JsValue::from_str("part without a body"))?;
            JsFuture::from(body.pipe_to_with_options(&sink, &keep_open)).await?;
        }
        JsFuture::from(sink.close()).await?;
        Ok(())
    }

    /// Whether the browser can inflate the parts.
    fn can_inflate() -> bool {
        js_sys::Reflect::has(&js_sys::global(), &JsValue::from_str("DecompressionStream")).unwrap_or(false)
    }

    /// `compressed` inflated by the browser.
    fn inflated(compressed: &ReadableStream) -> Result<ReadableStream, JsValue> {
        let inflater = DecompressionStream::new("gzip")?;
        Ok(compressed.pipe_through(&ReadableWritablePair::new(&inflater.readable(), &inflater.writable())))
    }

    async fn run(fresh: bool) -> Result<(), JsValue> {
        let list = JsFuture::from(fetch(PARTS, fresh).await?.text()?).await?.as_string().unwrap_or_default();
        let parts = super::parts(&list).ok_or_else(|| JsValue::from_str("app/parts.txt is not a parts list"))?;
        let joined = TransformStream::new()?;
        let wasm = inflated(&joined.readable())?;
        // Why the download stopped: the app's loader only reports that its stream broke.
        let cause = Rc::new(RefCell::new(None));
        let failed = cause.clone();
        let sink = joined.writable();
        spawn_local(async move {
            if let Err(e) = feed(parts, sink.clone(), fresh).await {
                let _ = sink.abort_with_reason(&e);
                failed.replace(Some(e));
            }
        });
        let headers = Headers::new()?;
        headers.set("Content-Type", "application/wasm")?;
        let init = ResponseInit::new();
        init.set_headers(&headers);
        let response = Response::new_with_opt_readable_stream_and_init(Some(&wasm), &init)?;
        let arg = js_sys::Object::new();
        js_sys::Reflect::set(&arg, &JsValue::from_str("module_or_path"), &response)?;
        match start_app(arg.into()).await {
            Ok(_) => Ok(()),
            Err(e) => Err(cause.take().unwrap_or(e)),
        }
    }

    async fn sleep(seconds: u32) {
        let wait = js_sys::Promise::new(&mut |done, _| {
            let ms = i32::try_from(seconds.saturating_mul(1000)).unwrap_or(i32::MAX);
            if window().and_then(|w| w.set_timeout_with_callback_and_timeout_and_arguments_0(&done, ms)).is_err() {
                let _ = done.call0(&JsValue::NULL);
            }
        });
        let _ = JsFuture::from(wait).await;
    }

    /// Replaces the page's "Loading" text. `reload` adds a link that reloads the page.
    fn say(text: &str, reload: bool) {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
        let Some(loading) = document.get_element_by_id("photocraft_loading") else { return };
        loading.set_text_content(None);
        let Ok(line) = document.create_element("p") else { return };
        line.set_text_content(Some(text));
        if reload && let Ok(link) = document.create_element("a") {
            link.set_text_content(Some("Reload"));
            let _ = link.set_attribute("href", "");
            let _ = line.append_child(&link);
        }
        let _ = loading.append_child(&line);
    }

    pub fn start() {
        if !can_inflate() {
            // Trying again can't help, and neither can a reload.
            say("PhotoCraft needs a newer browser: Safari 16.4, Firefox 113, Chrome 80 or later.", false);
            return;
        }
        spawn_local(async {
            let mut attempt = 0;
            loop {
                let error = match run(attempt > 0).await {
                    Ok(()) => return,
                    Err(e) => e,
                };
                web_sys::console::error_2(&JsValue::from_str("photocraft-web-boot: could not start PhotoCraft:"), &error);
                let Some(&wait) = RETRY_AFTER.get(attempt) else {
                    say("PhotoCraft couldn't be downloaded. Check your connection, then try again: ", true);
                    return;
                };
                say("Loading PhotoCraft\u{2026} (the download failed, trying again)", false);
                sleep(wait).await;
                attempt += 1;
            }
        });
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {
    boot::start();
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("photocraft-web-boot only runs in the browser: packaging/web/package.sh builds it with `trunk build --release` in apps/photocraft-web-boot");
}

#[cfg(test)]
mod tests {
    use super::{MARKER, parts};

    #[test]
    fn a_parts_list_gives_its_names_in_order() {
        let list = format!("{MARKER}\nphotocraft-web-0123abcd.part-aa\r\n\n  photocraft-web-0123abcd.part-ab  \n");
        assert_eq!(parts(&list), Some(vec!["photocraft-web-0123abcd.part-aa".to_owned(), "photocraft-web-0123abcd.part-ab".to_owned()]));
    }

    /// A host that answers a missing file with its home page must not have that page read as a
    /// list of parts.
    #[test]
    fn a_web_page_is_not_a_parts_list() {
        assert_eq!(parts(include_str!("../index.html")), None);
        assert_eq!(parts(""), None);
        assert_eq!(parts("photocraft-web.part-aa\n"), None, "no marker");
        assert_eq!(parts(&format!("{MARKER}\n")), None, "no parts");
        assert_eq!(parts("photocraft-parts 2\na.part-aa\n"), None, "a later layout this build doesn't know");
    }

    /// Part names are joined onto `./app/`, so one that could leave that folder, or name another
    /// origin, refuses the whole list.
    #[test]
    fn a_part_name_cannot_point_outside_the_app_folder() {
        for bad in ["../index.html", "a/b.part-aa", "a\\b", "https://example.com/x", "//example.com/x", "x?y", "x#y", ".hidden", "%2e%2e"] {
            assert_eq!(parts(&format!("{MARKER}\ngood.part-aa\n{bad}\n")), None, "{bad}");
        }
    }

    /// The packaged site shows this crate's page, the development build the app's own. They are
    /// the same page apart from which program the `<link data-trunk>` line builds.
    #[test]
    fn the_page_is_the_apps_page() {
        let trunk = |html: &str| html.lines().filter(|l| !l.contains("data-trunk")).collect::<Vec<_>>().join("\n");
        let (boot, app) = (include_str!("../index.html"), include_str!("../../photocraft-web/index.html"));
        assert_eq!(trunk(boot), trunk(app));
        assert!(boot.contains(r#"data-bin="photocraft-web-boot""#) && boot.contains(r#"data-cargo-profile="wasm-release""#), "{boot}");
        assert!(boot.contains(r#"id="photocraft_loading""#), "the element the failure message replaces");
    }
}

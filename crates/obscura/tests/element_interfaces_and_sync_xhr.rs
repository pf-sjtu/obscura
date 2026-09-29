// Regression coverage for two upstream issues:
//
// - HTML element interfaces were all aliased to `Element`
//   (`globalThis.HTMLIFrameElement = Element` and friends), so every brand
//   check matched every element: `div instanceof HTMLIFrameElement` was true.
//   Webpack's style-loader gates insertion on exactly that check and could
//   not install a single stylesheet (pages rendered unstyled). Each interface
//   is now its own class extending HTMLElement.
// - `XMLHttpRequest.open(method, url, false)` ignored the sync flag and
//   routed through async fetch, so send() never completed from the script's
//   point of view (status 0, empty body). Sync XHR now performs the request
//   on a worker thread while the isolate blocks, like a real browser.

use std::io::{Read, Write};

use obscura::Browser;

fn spawn_server() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else {
                continue;
            };
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let target = request.split_whitespace().nth(1).unwrap_or("").to_string();
                let body: Vec<u8> = if target.starts_with("/probe.json") {
                    br#"{"ok":true}"#.to_vec()
                } else {
                    br#"<!doctype html><html><head><title>fixture</title></head><body>
<dialog open id="dlg" style="display:flex;flex-direction:column;width:320px">
  <div style="flex:1 1 0;overflow:auto"><p>content line one</p><p>content line two</p></div>
  <div style="display:flex;gap:8px"><button>ok</button><button>cancel</button></div>
</dialog>
<div id="outer" style="display:flex;flex-direction:column">
  <div id="mid" style="display:flex;flex-direction:column;flex:1 1 0;overflow:auto">
    <div id="leaf" style="display:flex"><span>deep content</span><span>more</span></div>
  </div>
</div>
</body></html>"#
                        .to_vec()
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len(),
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.shutdown(std::net::Shutdown::Both);
            });
        }
    });
    format!("http://{}", addr)
}

#[tokio::test(flavor = "current_thread")]
async fn html_element_interfaces_have_distinct_brands() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server();

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();

    let probes = page.evaluate(
        r#"(function () {
            var div = document.createElement('div');
            return {
                div_is_div: div instanceof HTMLDivElement,
                div_not_iframe: !(div instanceof HTMLIFrameElement),
                div_not_anchor: !(div instanceof HTMLAnchorElement),
                div_is_html_el: div instanceof HTMLElement,
                div_is_el: div instanceof Element,
                iframe_is_iframe: document.createElement('iframe') instanceof HTMLIFrameElement,
                iframe_is_el: document.createElement('iframe') instanceof Element,
                body_is_body: document.body instanceof HTMLBodyElement,
                head_is_head: document.head instanceof HTMLHeadElement,
                h1_is_heading: document.createElement('h1') instanceof HTMLHeadingElement,
                table_is_table: document.createElement('table') instanceof HTMLTableElement,
                button_is_button: document.createElement('button') instanceof HTMLButtonElement,
                unknown_is_unknown: document.createElement('zqfoo') instanceof HTMLUnknownElement,
                unknown_is_html_el: document.createElement('zqfoo') instanceof HTMLElement,
                custom_is_html_el: document.createElement('x-y-z') instanceof HTMLElement,
                main_is_html_el: document.createElement('main') instanceof HTMLElement,
                svg_is_svg: document.createElementNS('http://www.w3.org/2000/svg','svg') instanceof SVGSVGElement,
                svg_is_not_html_el: !(document.createElementNS('http://www.w3.org/2000/svg','svg') instanceof HTMLElement),
                canvas_is_canvas: document.createElement('canvas') instanceof HTMLCanvasElement,
                img_is_img: document.createElement('img') instanceof HTMLImageElement,
            };
        })()"#,
    );

    for (k, v) in probes.as_object().expect("probe object") {
        assert_eq!(*v, serde_json::Value::Bool(true), "{k} was false");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn synchronous_xhr_blocks_and_returns_the_response() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server();

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();

    let probes = page.evaluate(
        r#"(function () {
            var states = [];
            var x = new XMLHttpRequest();
            x.onreadystatechange = function () { states.push(x.readyState); };
            x.open('GET', 'probe.json', false);
            x.send();
            return {
                status: x.status,
                ready_state: x.readyState,
                states_seen: states,
                body: x.responseText,
                response: x.response,
            };
        })()"#,
    );

    assert_eq!(probes["status"], 200, "sync XHR must complete before send() returns");
    assert_eq!(probes["ready_state"], 4);
    assert_eq!(probes["body"], r#"{"ok":true}"#);
    assert_eq!(probes["response"], r#"{"ok":true}"#);
}

#[cfg(feature = "render")]
#[tokio::test(flavor = "current_thread")]
async fn flex_dialog_and_nested_intrinsic_flex_get_positive_boxes() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server();

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();
    // Let layout settle so retained geometry is populated.
    page.settle(500).await;

    let probes = page.evaluate(
        r#"(function () {
            function box(sel) {
                var r = document.querySelector(sel).getBoundingClientRect();
                return [r.width.toFixed(0), r.height.toFixed(0)];
            }
            return {
                dialog: box('#dlg'),
                dialog_display: getComputedStyle(document.getElementById('dlg')).display,
                outer: box('#outer'),
                mid: box('#mid'),
                leaf: box('#leaf'),
            };
        })()"#,
    );

    for (k, v) in probes.as_object().expect("probe object") {
        if k == "dialog_display" {
            assert_eq!(v, "flex");
            continue;
        }
        let dims = v.as_array().expect("box dims");
        let w = dims[0].as_str().unwrap().parse::<f32>().unwrap();
        let h = dims[1].as_str().unwrap().parse::<f32>().unwrap();
        assert!(w > 10.0 && h > 10.0, "{k} collapsed to {w}x{h}");
    }
}

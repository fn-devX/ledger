use std::collections::HashMap;

use opentelemetry::{
    Context, global,
    propagation::{Extractor, Injector},
    trace::TraceContextExt,
};
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

pub type Carrier = HashMap<String, String>;

pub fn inject(span: &Span) -> Carrier {
    inject_context(&span.context())
}

pub fn current() -> Carrier {
    inject(&Span::current())
}

pub fn inject_context(cx: &Context) -> Carrier {
    let mut carrier = Carrier::new();
    global::get_text_map_propagator(|p| p.inject_context(cx, &mut carrier));
    carrier
}

pub fn extract(carrier: &dyn Extractor) -> Context {
    global::get_text_map_propagator(|p| p.extract(carrier))
}

pub fn set_parent(span: &Span, carrier: &dyn Extractor) {
    let cx = extract(carrier);
    if cx.span().span_context().is_valid() {
        let _ = span.set_parent(cx);
    }
}

pub fn trace_id(span: &Span) -> Option<String> {
    let cx = span.context();
    let sc = cx.span().span_context().clone();
    sc.is_valid().then(|| sc.trace_id().to_string())
}

pub struct HeaderExtractor<'a>(pub &'a http::HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(http::HeaderName::as_str).collect()
    }
}

pub struct HeaderInjector<'a>(pub &'a mut http::HeaderMap);

impl Injector for HeaderInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        if let (Ok(name), Ok(value)) = (
            http::HeaderName::from_bytes(key.as_bytes()),
            http::HeaderValue::from_str(&value),
        ) {
            self.0.insert(name, value);
        }
    }
}

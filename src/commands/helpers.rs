use std::borrow::Cow;

pub use edgeql_parser::helpers::quote_name;

pub fn quote_namespaced(name: &str) -> Cow<'_, str> {
    if name.contains("::") {
        let mut buf = String::with_capacity(name.len());
        let mut iter = name.split("::");
        buf.push_str(&quote_name(iter.next().unwrap()));
        for chunk in iter {
            buf.push_str("::");
            buf.push_str(&quote_name(chunk));
        }
        buf.into()
    } else {
        quote_name(name)
    }
}

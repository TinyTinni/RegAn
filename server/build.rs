use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::Path;

use lightningcss::rules::CssRule;
use lightningcss::selector::Component;
use lightningcss::stylesheet::{ParserOptions, PrinterOptions, StyleSheet};

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let html = std::fs::read_to_string(root.join("static/index.html"))
        .expect("Failed to read static/index.html");
    let css = std::fs::read_to_string(root.join("static/picnic.min.css"))
        .expect("Failed to read static/picnic.min.css");

    let out = std::env::var("OUT_DIR").unwrap();
    let is_release = std::env::var("PROFILE").unwrap() == "release";

    if is_release {
        let used_classes = extract_used_classes(&html);
        let filtered_css = tree_shake_css(&css, &used_classes);

        let mut cfg = minify_html::Cfg::new();
        cfg.minify_css = true;
        cfg.minify_js = true;
        let minified_html = minify_html::minify(html.as_bytes(), &cfg);
        let minified_html = strip_whitespace(&minified_html);

        let out = Path::new(&out);
        std::fs::write(out.join("index.html"), &minified_html).expect("Failed to write index.html");
        std::fs::write(out.join("picnic.min.css"), filtered_css.as_bytes())
            .expect("Failed to write picnic.min.css");
        std::fs::write(out.join("index.html.etag"), compute_etag(&minified_html))
            .expect("Failed to write index.html.etag");
        std::fs::write(
            out.join("picnic.min.css.etag"),
            compute_etag(filtered_css.as_bytes()),
        )
        .expect("Failed to write picnic.min.css.etag");

        let orig = css.len();
        let after = filtered_css.len();
        let saved = orig - after;
        let pct = saved as f64 / orig as f64 * 100.0;
        println!(
            "cargo::warning=CSS tree-shake: {orig} B -> {after} B ({saved} B, {pct:.1}% saved)"
        );

        let h_orig = html.len();
        let h_after = minified_html.len();
        let h_saved = h_orig - h_after;
        let h_pct = h_saved as f64 / h_orig as f64 * 100.0;
        println!(
            "cargo::warning=HTML minify: {h_orig} B -> {h_after} B ({h_saved} B, {h_pct:.1}% saved)"
        );
    } else {
        std::fs::write(Path::new(&out).join("index.html"), html.as_bytes())
            .expect("Failed to write index.html");
        std::fs::write(Path::new(&out).join("picnic.min.css"), css.as_bytes())
            .expect("Failed to write picnic.min.css");
    }

    println!(
        "cargo::rerun-if-changed={}",
        root.join("static/index.html").display()
    );
    println!(
        "cargo::rerun-if-changed={}",
        root.join("static/picnic.min.css").display()
    );
}

// ---------------------------------------------------------------------------
// CSS tree-shaking
// ---------------------------------------------------------------------------

fn extract_used_classes(html: &str) -> HashSet<String> {
    let mut used = HashSet::new();
    for cap in html.match_indices("class=\"") {
        let start = cap.0 + 7;
        if let Some(end) = html[start..].find('"') {
            for cls in html[start..start + end].split_whitespace() {
                used.insert(cls.to_string());
            }
        }
    }
    for cap in html.match_indices("id=\"") {
        let start = cap.0 + 4;
        if let Some(end) = html[start..].find('"')
            && !html[start..start + end].is_empty()
        {
            used.insert(html[start..start + end].to_string());
        }
    }
    used
}

fn tree_shake_css(css: &str, used: &HashSet<String>) -> String {
    let mut stylesheet =
        StyleSheet::parse(css, ParserOptions::default()).expect("Failed to parse CSS");
    filter_rules(&mut stylesheet.rules.0, used);
    stylesheet
        .to_css(PrinterOptions {
            minify: true,
            ..PrinterOptions::default()
        })
        .expect("Failed to serialize CSS")
        .code
}

fn filter_rules(rules: &mut Vec<CssRule>, used: &HashSet<String>) {
    for rule in rules.iter_mut() {
        if let CssRule::Media(media) = rule {
            filter_rules(&mut media.rules.0, used);
        }
    }
    rules.retain(|rule| match rule {
        CssRule::Style(style) => {
            let mut has_class = false;
            for selector in &style.selectors.0 {
                for component in selector.iter_raw_match_order() {
                    if let Component::Class(ident) = component {
                        has_class = true;
                        if used.contains(ident.0.as_ref()) {
                            return true;
                        }
                    }
                }
            }
            !has_class
        }
        CssRule::Media(media) => !media.rules.0.is_empty(),
        _ => true,
    });
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn compute_etag(content: &[u8]) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    format!("\"{:016x}\"", hasher.finish())
}

fn strip_whitespace(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for line in bytes.split(|&b| b == b'\n') {
        let trimmed = line
            .iter()
            .copied()
            .skip_while(|&b| b == b' ' || b == b'\t')
            .collect::<Vec<_>>();
        if trimmed.is_empty() {
            continue;
        }
        out.extend(&trimmed);
        out.push(b'\n');
    }
    while out.last() == Some(&b'\n') {
        out.pop();
    }
    out
}

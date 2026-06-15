//! XMP（RDF/XML）非校验式扫描：把包扫成 (prefix, name, value) 属性列表。
//! 不解析命名空间 URI，不做 DTD/CDATA；属性形式与元素形式（含 rdf:li）皆覆盖。

use alloc::string::String;
use alloc::vec::Vec;

use crate::limits::Limits;
use crate::model::{WarnKind, Warning, XmpProperty};

/// 把一段 XMP 包扫成属性列表。无效 UTF-8 → 一条 Truncated 告警后返回。
pub fn decode(
    packet: &[u8],
    out: &mut Vec<XmpProperty>,
    warnings: &mut Vec<Warning>,
    limits: &Limits,
) {
    if packet.len() > limits.max_payload_bytes {
        warnings.push(Warning { offset: 0, kind: WarnKind::Truncated });
        return;
    }
    let text = match core::str::from_utf8(packet) {
        Ok(t) => t,
        Err(_) => {
            warnings.push(Warning { offset: 0, kind: WarnKind::Truncated });
            return;
        }
    };
    scan_attributes(text, out, limits);
    scan_elements(text, out, limits);
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b':' | b'.' | b'_' | b'-')
}

/// 结构性前缀，不作为属性产出。
fn is_structural_prefix(px: &str) -> bool {
    matches!(px, "xmlns" | "rdf" | "xml" | "x")
}

fn split_prefix(qname: &str) -> Option<(&str, &str)> {
    let idx = qname.find(':')?;
    let (px, rest) = qname.split_at(idx);
    let nm = &rest[1..];
    if px.is_empty() || nm.is_empty() {
        return None;
    }
    Some((px, nm))
}

/// 解码五个基本 XML 实体，其余原样保留。
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return String::from(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        if let Some(semi) = tail.find(';') {
            let ent = &tail[1..semi];
            match ent {
                "lt" => out.push('<'),
                "gt" => out.push('>'),
                "amp" => out.push('&'),
                "quot" => out.push('"'),
                "apos" => out.push('\''),
                _ => out.push_str(&tail[..=semi]), // 未知实体原样保留
            }
            rest = &tail[semi + 1..];
        } else {
            out.push('&');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

fn push_prop(out: &mut Vec<XmpProperty>, limits: &Limits, prefix: &str, name: &str, value: &str) {
    if out.len() >= limits.max_tags {
        return;
    }
    out.push(XmpProperty {
        prefix: String::from(prefix),
        name: String::from(name),
        value: decode_entities(value),
    });
}

/// 扫描所有 `name="value"` / `name='value'` 属性对（任意元素上）。
fn scan_attributes(text: &str, out: &mut Vec<XmpProperty>, limits: &Limits) {
    let b = text.as_bytes();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'=' && (b[i + 1] == b'"' || b[i + 1] == b'\'') {
            let quote = b[i + 1];
            // 名字：跳过 = 前空白，再回退取名字符
            let mut ne = i;
            while ne > 0 && b[ne - 1].is_ascii_whitespace() {
                ne -= 1;
            }
            let mut ns = ne;
            while ns > 0 && is_name_byte(b[ns - 1]) {
                ns -= 1;
            }
            // 值
            let vs = i + 2;
            let mut ve = vs;
            while ve < b.len() && b[ve] != quote {
                ve += 1;
            }
            if ve >= b.len() {
                break;
            }
            if ns < ne {
                let name = &text[ns..ne];
                let value = &text[vs..ve];
                if let Some((px, nm)) = split_prefix(name)
                    && !is_structural_prefix(px)
                {
                    push_prop(out, limits, px, nm, value);
                }
            }
            i = ve + 1;
        } else {
            i += 1;
        }
    }
}

struct Frame<'a> {
    prefix: &'a str,
    name: &'a str,
    is_alt: bool,
    alt_taken: bool,
}

/// 扫描元素形式 `<prefix:name>text</...>` 与 rdf 容器中的 `rdf:li`。
fn scan_elements(text: &str, out: &mut Vec<XmpProperty>, limits: &Limits) {
    let b = text.as_bytes();
    let mut stack: Vec<Frame> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        if i + 1 >= b.len() {
            break;
        }
        // 注释 / PI / 声明：跳到 '>'
        if b[i + 1] == b'!' || b[i + 1] == b'?' {
            i = find_gt(b, i);
            continue;
        }
        // 闭合标签
        if b[i + 1] == b'/' {
            let (px, nm, end) = parse_qname(text, i + 2);
            if let Some(f) = stack.last()
                && f.prefix == px && f.name == nm
            {
                stack.pop();
            }
            i = find_gt(b, end);
            continue;
        }
        // 开始标签
        let (px, nm, after_name) = parse_qname(text, i + 1);
        let gt = find_gt(b, after_name);
        if gt >= b.len() {
            break; // 截断标签：无闭合 '>'
        }
        let self_closing = b[gt - 1] == b'/';
        let content_start = gt + 1; // '>' 之后
        if self_closing || px.is_empty() {
            i = content_start;
            continue;
        }
        // 看内容是否为纯文本叶子
        let mut j = content_start;
        while j < b.len() && b[j] != b'<' {
            j += 1;
        }
        let content = &text[content_start..j.min(b.len())];
        let is_leaf = j < b.len() && !content.trim().is_empty();
        if is_leaf {
            record_leaf(px, nm, content, &mut stack, out, limits);
            i = j; // 后续闭合标签由顶层处理（不匹配栈顶则忽略）
        } else {
            let is_alt = px == "rdf" && nm == "Alt";
            if stack.len() < limits.max_depth as usize {
                stack.push(Frame { prefix: px, name: nm, is_alt, alt_taken: false });
            }
            i = content_start;
        }
    }
}

fn record_leaf<'a>(
    px: &'a str,
    nm: &'a str,
    content: &str,
    stack: &mut [Frame<'a>],
    out: &mut Vec<XmpProperty>,
    limits: &Limits,
) {
    let val = content.trim();
    if px == "rdf" && nm == "li" {
        // 若直接容器是 rdf:Alt，只取首个
        if let Some(top) = stack.last_mut()
            && top.is_alt
        {
            if top.alt_taken {
                return;
            }
            top.alt_taken = true;
        }
        // 归属到最近的非 rdf 祖先属性名
        if let Some(prop) = stack.iter().rev().find(|f| f.prefix != "rdf") {
            push_prop(out, limits, prop.prefix, prop.name, val);
        }
    } else if px != "rdf" && px != "x" {
        push_prop(out, limits, px, nm, val);
    }
}

/// 从 `start`（'<' 或 '/' 之后）解析限定名，返回 (prefix, name, 名字结束后的索引)。
/// 无前缀时 prefix 为空串。
fn parse_qname(text: &str, start: usize) -> (&str, &str, usize) {
    let b = text.as_bytes();
    let mut e = start;
    while e < b.len() && is_name_byte(b[e]) {
        e += 1;
    }
    let qname = &text[start..e.min(b.len())];
    match split_prefix(qname) {
        Some((px, nm)) => (px, nm, e),
        None => ("", qname, e),
    }
}

/// 返回从 `from` 起第一个 '>' 的索引；找不到则返回 b.len()。
fn find_gt(b: &[u8], from: usize) -> usize {
    let mut k = from;
    while k < b.len() && b[k] != b'>' {
        k += 1;
    }
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(packet: &[u8]) -> (Vec<XmpProperty>, Vec<Warning>) {
        let mut out = Vec::new();
        let mut warns = Vec::new();
        decode(packet, &mut out, &mut warns, &Limits::default());
        (out, warns)
    }

    fn find<'a>(props: &'a [XmpProperty], prefix: &str, name: &str) -> Option<&'a str> {
        props
            .iter()
            .find(|p| p.prefix == prefix && p.name == name)
            .map(|p| p.value.as_str())
    }

    #[test]
    fn attribute_form() {
        let pkt = br#"<rdf:Description rdf:about="" xmlns:tiff="ns" tiff:Make="Acme" tiff:Orientation="6"/>"#;
        let (props, warns) = run(pkt);
        assert!(warns.is_empty());
        assert_eq!(find(&props, "tiff", "Make"), Some("Acme"));
        assert_eq!(find(&props, "tiff", "Orientation"), Some("6"));
        // 结构属性不应出现
        assert!(find(&props, "rdf", "about").is_none());
        assert!(find(&props, "xmlns", "tiff").is_none());
    }

    #[test]
    fn element_form_leaf() {
        let pkt = br#"<rdf:Description><tiff:Model>X100</tiff:Model></rdf:Description>"#;
        let (props, _) = run(pkt);
        assert_eq!(find(&props, "tiff", "Model"), Some("X100"));
    }

    #[test]
    fn rdf_alt_takes_first_li() {
        let pkt = br#"<dc:description><rdf:Alt><rdf:li xml:lang="x-default">hello</rdf:li><rdf:li xml:lang="fr">bonjour</rdf:li></rdf:Alt></dc:description>"#;
        let (props, _) = run(pkt);
        let vals: Vec<&str> = props
            .iter()
            .filter(|p| p.prefix == "dc" && p.name == "description")
            .map(|p| p.value.as_str())
            .collect();
        assert_eq!(vals, vec!["hello"]);
    }

    #[test]
    fn decodes_basic_entities() {
        let pkt = br#"<rdf:Description dc:rights="a &amp; b &lt;c&gt;"/>"#;
        let (props, _) = run(pkt);
        assert_eq!(find(&props, "dc", "rights"), Some("a & b <c>"));
    }

    #[test]
    fn invalid_utf8_warns_and_returns_empty() {
        let (props, warns) = run(&[0xFF, 0xFE, 0x00]);
        assert!(props.is_empty());
        assert_eq!(warns.len(), 1);
        assert_eq!(warns[0].kind, WarnKind::Truncated);
    }

    #[test]
    fn max_tags_caps_output() {
        let pkt = br#"<rdf:Description a:one="1" a:two="2" a:three="3"/>"#;
        let mut out = Vec::new();
        let mut warns = Vec::new();
        let limits = Limits { max_tags: 2, ..Limits::default() };
        decode(pkt, &mut out, &mut warns, &limits);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn truncated_tag_does_not_panic() {
        let (props, _warns) = run(b"<tiff:Make");
        assert!(props.is_empty());
    }

    #[test]
    fn oversized_packet_rejected() {
        let limits = Limits { max_payload_bytes: 16, ..Limits::default() };
        // 构造一个合法 UTF-8 但超过 max_payload_bytes 的包
        let pkt = b"<rdf:Description tiff:Make=\"Acme\"/>";
        assert!(pkt.len() > 16);
        let mut out = Vec::new();
        let mut warns = Vec::new();
        decode(pkt, &mut out, &mut warns, &limits);
        assert!(out.is_empty(), "oversized packet must produce no props");
        assert_eq!(warns.len(), 1);
        assert_eq!(warns[0].kind, WarnKind::Truncated);
        assert_eq!(warns[0].offset, 0);
    }

    #[test]
    fn deep_nesting_does_not_oom() {
        // 构造 500 层嵌套开标签，确认不 panic / OOM，且栈深受 max_depth 约束
        let mut pkt = Vec::new();
        for _ in 0..500 {
            pkt.extend_from_slice(b"<a:b>");
        }
        // 不需断言具体属性，只需正常返回即可
        let (_, _) = run(&pkt);
    }
}

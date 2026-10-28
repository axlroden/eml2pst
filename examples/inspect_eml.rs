//! Prints how an .eml file maps to a PST message: `cargo run --example inspect_eml -- FILE`.

fn main() {
    let path = std::env::args().nth(1).expect("usage: inspect_eml FILE");
    let raw = std::fs::read(&path).expect("read file");
    let msg = eml2pst::parse_eml(&raw).expect("not an email message");
    let head = |s: &Option<String>| {
        s.as_deref()
            .map(|s| s.chars().take(300).collect::<String>())
    };
    println!("class:       {:?}", msg.message_class);
    println!("subject:     {:?}", msg.subject);
    println!("from:        {:?}", msg.from);
    println!("sender:      {:?}", msg.sender);
    println!("recipients:  {}", msg.recipients.len());
    println!("sent/recv:   {:?} {:?}", msg.sent_time, msg.received_time);
    println!("body:        {:?}", head(&msg.body));
    println!("html bytes:  {:?}", msg.html.as_ref().map(String::len));
    for (key, value) in &msg.extra_props {
        let shown = match value {
            eml2pst::ltp::PropValue::Binary(b) => format!("Binary({})", hex(b)),
            v => format!("{v:?}"),
        };
        let key = match key {
            eml2pst::PropKey::Id(id) => format!("{id:04X}"),
            eml2pst::PropKey::Named(g, n) => format!("{{{}}} {n:?}", hex(&g[..4])),
        };
        let limit = if std::env::var_os("FULL").is_some() {
            usize::MAX
        } else {
            120
        };
        println!(
            "prop:        {key} = {}",
            shown.chars().take(limit).collect::<String>()
        );
    }
    for a in &msg.attachments {
        println!(
            "attachment:  {:?} {:?} cid={:?} inline={} {} bytes",
            a.filename,
            a.mime_type,
            a.content_id,
            a.inline,
            a.data.len()
        );
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect()
}

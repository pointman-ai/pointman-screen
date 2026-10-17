//! extension.toml within what core's manifest check takes (Pointman's src/messageboard/extensions/
//! manifest.py): core refuses a release whose manifest breaks one, as it did v0.1.0's (a command
//! summary over 120 characters).

#[test]
fn the_manifest_keeps_to_cores_limits() {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/extension.toml")).unwrap();
    let m: toml::Table = toml::from_str(&text).unwrap();
    let chars = |v: &toml::Value| v.as_str().unwrap().chars().count();
    let ext = &m["extension"];
    assert_eq!(ext["version"].as_str(), Some(env!("CARGO_PKG_VERSION")), "extension.toml's version is Cargo's");
    assert!((1..=60).contains(&chars(&ext["name"])) && (1..=200).contains(&chars(&ext["summary"])));
    for c in m["commands"].as_array().unwrap() {
        let kind = c["kind"].as_str().unwrap();
        assert!(kind.starts_with("screen."), "{kind}");
        assert!((1..=60).contains(&chars(&c["title"])), "{kind}'s title is {} characters", chars(&c["title"]));
        assert!(chars(&c["summary"]) <= 120, "{kind}'s summary is {} characters", chars(&c["summary"]));
        let slash = c["slash"].as_str().unwrap();
        let ok = slash.len() <= 24
            && slash.starts_with(|ch: char| ch.is_ascii_lowercase())
            && slash.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-');
        assert!(ok, "{kind}'s slash {slash}");
        assert_eq!(c["params"]["type"].as_str(), Some("object"), "{kind}");
    }
    for p in m["panes"].as_array().unwrap() {
        assert!((1..=60).contains(&chars(&p["title"])));
        assert_eq!(p["format"].as_str(), Some("video"));
    }
}

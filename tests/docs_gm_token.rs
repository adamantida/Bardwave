//! The shipped docs must tell the truth about the GM control page: a generated
//! `gm_token` opens it to remote clients by default, so the README, the
//! packaging text and the example config have to describe `?g=<gm_token>` and
//! the `gm_token = ""` switch instead of calling the page local-only.

fn read(rel: &str) -> String {
    std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel))
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
}

#[test]
fn docs_describe_remote_gm_access_and_how_to_disable_it() {
    for rel in ["README.md", "packaging/README.txt", "packaging/config.toml"] {
        let text = read(rel);
        assert!(text.contains("gm_token"), "{rel} does not mention gm_token");
    }
    for rel in ["README.md", "packaging/README.txt"] {
        let text = read(rel);
        assert!(text.contains("?g="), "{rel} does not describe the ?g= link");
        assert!(
            text.contains(r#"gm_token = """#),
            "{rel} does not say how to turn remote control off"
        );
    }
}

#[test]
fn docs_do_not_claim_the_control_page_is_local_only() {
    for rel in ["README.md", "packaging/README.txt"] {
        let text = read(rel).replace('\n', " ");
        assert!(
            !text.contains("only answers on the machine that runs Bardwave"),
            "{rel} still says the control page is local-only"
        );
        assert!(
            !text.contains("works only on this PC"),
            "{rel} still says the control page works only on this PC"
        );
    }
}

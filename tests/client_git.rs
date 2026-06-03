use denia::cli::client::git::{parse_branch_name, remote_head_matches};

#[test]
fn parse_branch_name_trims_stdout() {
    assert_eq!(parse_branch_name("main\n").unwrap(), "main");
}

#[test]
fn parse_branch_name_rejects_empty() {
    assert!(parse_branch_name("\n").is_err());
}

#[test]
fn remote_head_match_compares_hashes() {
    assert!(remote_head_matches("abc\n", "abc\n"));
    assert!(!remote_head_matches("abc\n", "def\n"));
}

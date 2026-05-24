#[test]
#[ignore = "requires root, cgroup v2, and Linux namespace permissions"]
fn privileged_runtime_tests_are_explicitly_gated() {
    assert_eq!(
        std::env::var("DENIA_RUN_PRIVILEGED_TESTS").as_deref(),
        Ok("1")
    );
}

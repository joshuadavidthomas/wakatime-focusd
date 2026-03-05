#[test]
fn cli_tests() {
    trycmd::TestCases::new()
        .case("tests/cmd/*.trycmd")
        .insert_var("[VERSION]", env!("CARGO_PKG_VERSION"))
        .unwrap();
}

#[test]
fn tests() {
    let t = trybuild::TestCases::new();

    t.pass("tests/asyncops.rs");
    t.pass("tests/syncops.rs");
}

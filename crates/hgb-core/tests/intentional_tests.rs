#[test]
fn test_actually_flaky() {
    let iter = match std::env::var("HGB_FLAKY_ITER") {
        Ok(v) => v.parse::<usize>().unwrap_or(0),
        Err(_) => return, // Pass normally if not run via the FlakyExterminator
    };
    
    if iter % 2 == 0 {
        panic!("Flaky failure!");
    }
}

#[test]
fn test_actually_solid() {
    assert_eq!(1 + 1, 2);
}

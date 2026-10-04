use hgb_core::visionary::*;

#[test]
fn test_p2p_grid_join() {
    let grid = PeerToPeerGrid::new(500);
    let peers = grid.join_grid("hgb://127.0.0.1").unwrap();
    assert_eq!(peers, 0);
}

#[test]
fn test_ambient_learning_thread() {
    let learner = AmbientLearningThread::new(true, false);
    assert!(learner.start_learning_thread().is_ok());
}

#[test]
fn test_merkle_ledger_commit() {
    let ledger = MerkleLedger::new(true);
    let hash = ledger.verify_and_commit("fn main() {}").unwrap();
    assert!(hash.starts_with("0x"));
}

#[test]
fn test_ghost_overlay_spawn() {
    let overlay = GhostOverlayGui::new(true);
    assert!(overlay.spawn_hud().is_ok());
}

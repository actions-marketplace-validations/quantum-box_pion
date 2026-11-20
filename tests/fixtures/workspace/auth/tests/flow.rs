use fx_auth::repo::MemoryRepository;

#[test]
fn sign_up_flow() {
    assert!(fx_auth::sign_up(&MemoryRepository, 2));
}

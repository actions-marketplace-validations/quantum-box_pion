pub mod router;

use fx_auth::repo::MemoryRepository;

pub fn sign_in_handler(id: u32) -> bool {
    fx_auth::SignIn {
        repo: &MemoryRepository,
    }
    .execute(id)
}

pub fn sign_up_handler(id: u32) -> bool {
    fx_auth::sign_up(&MemoryRepository, id)
}

pub fn health_handler(_id: u32) -> bool {
    true
}

pub fn api() {
    fx_auth::auth()
}

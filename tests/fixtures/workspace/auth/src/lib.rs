pub mod repo;

use fx_core::token::Token;
use repo::UserRepository;

pub struct SignIn<'a> {
    pub repo: &'a dyn UserRepository,
}

impl SignIn<'_> {
    pub fn execute(&self, id: u32) -> bool {
        let user = self.repo.find(id);
        user.is_some() && !Token { age: id }.is_expired()
    }
}

pub fn sign_up(repo: &dyn UserRepository, id: u32) -> bool {
    repo.insert(id)
}

pub fn auth() {
    fx_core::core()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::MemoryRepository;

    #[test]
    fn rejects_expired_token() {
        let repo = MemoryRepository;
        assert!(!SignIn { repo: &repo }.execute(9));
    }

    #[test]
    fn creates_user() {
        fx_tools::tools();
        let repo = MemoryRepository;
        assert!(sign_up(&repo, 1));
    }
}

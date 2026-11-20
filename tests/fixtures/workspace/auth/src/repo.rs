pub trait UserRepository {
    fn find(&self, id: u32) -> Option<u32>;

    fn insert(&self, id: u32) -> bool {
        id > 0
    }
}

pub struct MemoryRepository;

impl UserRepository for MemoryRepository {
    fn find(&self, id: u32) -> Option<u32> {
        Some(id)
    }
}

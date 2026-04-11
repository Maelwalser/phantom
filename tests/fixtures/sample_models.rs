struct User {
    id: u64,
    name: String,
}

impl User {
    fn new(id: u64, name: String) -> Self {
        Self { id, name }
    }
}

struct Post {
    id: u64,
    title: String,
    author_id: u64,
}

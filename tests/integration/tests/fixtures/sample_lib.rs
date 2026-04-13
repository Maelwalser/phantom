use std::collections::HashMap;

const MAX_RETRIES: u32 = 3;

fn compute() -> i32 {
    42
}

struct Config {
    values: HashMap<String, String>,
}

impl Config {
    fn new() -> Self {
        Self {
            values: HashMap::new(),
        }
    }
}

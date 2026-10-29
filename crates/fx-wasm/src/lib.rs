use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn ready() -> bool {
    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_links() {
        assert!(super::ready());
    }
}

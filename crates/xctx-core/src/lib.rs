pub const APP_NAME: &str = "xctx";

pub fn placeholder_message() -> &'static str {
    "xctx skeleton"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_message_mentions_app_name() {
        assert!(placeholder_message().contains(APP_NAME));
    }
}

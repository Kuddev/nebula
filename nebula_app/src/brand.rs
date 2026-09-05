pub const NAME: &str = "Pebrel";
pub const DESCRIPTION: &str = "Pebrel — a GPU-accelerated terminal emulator";

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    #[test]
    fn display_name_and_cli_keep_the_compatibility_command() {
        let mut command = crate::cli::Options::command();
        assert_eq!(command.get_name(), super::NAME);
        assert_eq!(command.get_bin_name(), Some("nebula"));
        assert!(command.render_version().starts_with(super::NAME));
        assert!(command.render_long_help().to_string().contains(super::DESCRIPTION));
        assert_eq!(crate::config::window::Identity::default().title, super::NAME);
        assert_eq!(crate::config::window::DEFAULT_CLASS, "Nebula");
    }
}
